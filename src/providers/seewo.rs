//! Seewo AI Open Platform provider.
//!
//! Seewo uses an OpenAI-compatible chat format behind a custom authentication
//! layer: requests carry `x-open-appId`, `x-open-accessToken`, and `x-open-userId`
//! headers.  Access tokens are obtained via a signed token endpoint and cached
//! with automatic refresh on expiry.

use crate::multimodal;
use crate::providers::compatible::sse_bytes_to_events;
use crate::providers::traits::{
    ChatMessage, ChatRequest as ProviderChatRequest, ChatResponse as ProviderChatResponse,
    Provider, ProviderCapabilities, StreamEvent, StreamOptions, StreamResult, TokenUsage,
    ToolCall as ProviderToolCall,
};
use crate::tools::ToolSpec;
use async_trait::async_trait;
use futures_util::{StreamExt, stream};
use md5::{Digest, Md5};
use reqwest::Client;
use serde::Deserialize;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::RwLock;

// ── Seewo API endpoints ────────────────────────────────────────
const TOKEN_URL: &str = "https://openapi.seewo.com/api/v1/token/access";
const CHAT_BASE_URL: &str = "http://ai-open.seewo.com/api/v1/nlp/raw/chat/completion";
const STREAM_BASE_URL: &str = "http://ai-open.seewo.com/api/v1/nlp/raw/chat/completion/stream";

// ── Credentials (hardcoded for now; move to env/config later) ──
const APP_ID: &str = "ee741652710344a2b250bd18f00a5fd5";
const APP_SECRET: &str = "EgdDUNcGO0lNVYXAQuzuVrm2OkJ7sq8l";
const USER_ID: &str = "olxhwhxsuglwzguwhozgkivw263277p1";

/// Refresh token 60 seconds before its actual expiry.
const TOKEN_REFRESH_BUFFER_SECS: u64 = 60;

// ── Token cache ────────────────────────────────────────────────

struct CachedToken {
    access_token: String,
    expires_at: Instant,
}

impl CachedToken {
    fn is_expired(&self) -> bool {
        Instant::now() >= self.expires_at
    }
}

// ── Provider ───────────────────────────────────────────────────

pub struct SeewoProvider {
    client: Client,
    token_cache: Arc<RwLock<Option<CachedToken>>>,
}

impl SeewoProvider {
    pub fn new() -> Self {
        Self {
            client: crate::config::build_runtime_proxy_client_with_timeouts(
                "provider.seewo",
                120,
                10,
            ),
            token_cache: Arc::new(RwLock::new(None)),
        }
    }

    /// Return a valid access token, refreshing if expired.
    async fn ensure_token(&self) -> anyhow::Result<String> {
        {
            let cache = self.token_cache.read().await;
            if let Some(token) = cache.as_ref() {
                if !token.is_expired() {
                    return Ok(token.access_token.clone());
                }
            }
        }

        let mut cache = self.token_cache.write().await;
        // Double-check after acquiring write lock.
        if let Some(token) = cache.as_ref() {
            if !token.is_expired() {
                return Ok(token.access_token.clone());
            }
        }

        let new_token = self.fetch_token().await?;
        let access_token = new_token.access_token.clone();
        *cache = Some(new_token);
        Ok(access_token)
    }

    /// Call the Seewo token endpoint to obtain a fresh access token.
    async fn fetch_token(&self) -> anyhow::Result<CachedToken> {
        #[allow(clippy::cast_possible_truncation)]
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis() as u64;

        let sign = Self::compute_sign(timestamp);

        let resp = self
            .client
            .post(TOKEN_URL)
            .header("x-open-sign", &sign)
            .header("x-open-grantType", "authorization")
            .header("x-open-timestamp", timestamp.to_string())
            .header("x-open-appId", APP_ID)
            .header("Content-Type", "application/json")
            .body("{}")
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(super::api_error("Seewo (token)", resp).await);
        }

        let body: TokenResponse = resp.json().await?;

        let token_body = body.body.ok_or_else(|| {
            anyhow::anyhow!(
                "Seewo token response missing body: {}",
                body.message.unwrap_or_default()
            )
        })?;

        let expire_secs = token_body.expire_time.unwrap_or(7200);
        let buffer = TOKEN_REFRESH_BUFFER_SECS.min(expire_secs / 2);
        let expires_at = Instant::now() + std::time::Duration::from_secs(expire_secs - buffer);

        Ok(CachedToken {
            access_token: token_body.access_token,
            expires_at,
        })
    }

    /// `MD5("appId={APP_ID}&appSecret={APP_SECRET}&timestamp={ts}")`
    fn compute_sign(timestamp: u64) -> String {
        let input = format!("appId={APP_ID}&appSecret={APP_SECRET}&timestamp={timestamp}");
        let hash = Md5::digest(input.as_bytes());
        hash.iter().fold(String::with_capacity(32), |mut acc, b| {
            use std::fmt::Write;
            let _ = write!(acc, "{b:02x}");
            acc
        })
    }

    fn build_chat_url(base: &str, model: &str) -> String {
        format!("{base}?model={model}")
    }

    /// Convert `ToolSpec` list to OpenAI-format tool definitions.
    fn convert_tools(tools: Option<&[ToolSpec]>) -> Option<Vec<serde_json::Value>> {
        tools.map(|items| {
            items
                .iter()
                .map(|t| {
                    serde_json::json!({
                        "type": "function",
                        "function": {
                            "name": t.name,
                            "description": t.description,
                            "parameters": t.parameters,
                        }
                    })
                })
                .collect()
        })
    }

    /// Build message list from `ChatMessage` slice, handling tool-call history
    /// and vision content markers.
    fn convert_messages(messages: &[ChatMessage]) -> Vec<serde_json::Value> {
        messages
            .iter()
            .map(|m| {
                // Assistant messages with embedded tool_calls JSON.
                if m.role == "assistant" {
                    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&m.content) {
                        if value.get("tool_calls").is_some() {
                            return value_with_role("assistant", &value);
                        }
                    }
                }

                // Tool result messages.
                if m.role == "tool" {
                    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&m.content) {
                        let tool_call_id = value.get("tool_call_id").and_then(|v| v.as_str());
                        let content = value.get("content").and_then(|v| v.as_str());
                        return serde_json::json!({
                            "role": "tool",
                            "tool_call_id": tool_call_id,
                            "content": content,
                        });
                    }
                }

                // User messages — expand image markers into multipart content.
                if m.role == "user" {
                    let (cleaned, image_refs) = multimodal::parse_image_markers(&m.content);
                    if !image_refs.is_empty() {
                        let mut parts: Vec<serde_json::Value> = image_refs
                            .into_iter()
                            .map(|data_uri| {
                                serde_json::json!({"type": "image_url", "image_url": data_uri})
                            })
                            .collect();
                        if !cleaned.trim().is_empty() {
                            parts.push(serde_json::json!({"type": "text", "text": cleaned.trim()}));
                        }
                        return serde_json::json!({"role": "user", "content": parts});
                    }
                }

                // Plain text message.
                serde_json::json!({"role": m.role, "content": m.content})
            })
            .collect()
    }

    /// Build the JSON request body (model excluded — it goes in the query string).
    fn build_request_body(
        messages: &[serde_json::Value],
        tools: Option<&Vec<serde_json::Value>>,
        temperature: f64,
    ) -> serde_json::Value {
        let mut body = serde_json::json!({
            "messages": messages,
            "temperature": temperature,
            "user": USER_ID,
        });

        if let Some(tools) = tools {
            body["tools"] = serde_json::json!(tools);
        }

        body
    }

    fn parse_response(raw: NativeChatResponse) -> anyhow::Result<ProviderChatResponse> {
        let usage = raw.usage.map(|u| TokenUsage {
            input_tokens: u.prompt_tokens,
            output_tokens: u.completion_tokens,
            cached_input_tokens: u.prompt_tokens_details.and_then(|d| d.cached_tokens),
        });

        let choice = raw
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("No response from Seewo"))?;

        let text = match &choice.message.content {
            Some(c) if !c.is_empty() => Some(c.clone()),
            _ => choice.message.reasoning_content.clone(),
        };

        let tool_calls = choice
            .message
            .tool_calls
            .unwrap_or_default()
            .into_iter()
            .map(|tc| ProviderToolCall {
                id: tc.id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
                name: tc.function.name,
                arguments: tc.function.arguments,
            })
            .collect();

        Ok(ProviderChatResponse {
            text,
            tool_calls,
            usage,
            reasoning_content: choice.message.reasoning_content,
        })
    }
}

/// Merge `role` into an existing JSON value that already has content/tool_calls fields.
fn value_with_role(role: &str, value: &serde_json::Value) -> serde_json::Value {
    let mut v = value.clone();
    if let Some(obj) = v.as_object_mut() {
        obj.insert(
            "role".to_string(),
            serde_json::Value::String(role.to_string()),
        );
    }
    v
}

// ── API response types ─────────────────────────────────────────

#[derive(Deserialize)]
struct TokenResponse {
    body: Option<TokenBody>,
    #[serde(default)]
    message: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TokenBody {
    access_token: String,
    #[serde(default)]
    expire_time: Option<u64>,
}

#[derive(Deserialize)]
struct NativeChatResponse {
    choices: Vec<NativeChoice>,
    #[serde(default)]
    usage: Option<UsageInfo>,
}

#[derive(Deserialize)]
struct NativeChoice {
    message: NativeResponseMessage,
}

#[derive(Deserialize)]
struct NativeResponseMessage {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    reasoning_content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<NativeToolCall>>,
}

#[derive(Deserialize)]
struct NativeToolCall {
    #[serde(default)]
    id: Option<String>,
    function: NativeFunctionCall,
}

#[derive(Deserialize)]
struct NativeFunctionCall {
    name: String,
    arguments: String,
}

#[derive(Deserialize)]
struct UsageInfo {
    #[serde(default)]
    prompt_tokens: Option<u64>,
    #[serde(default)]
    completion_tokens: Option<u64>,
    #[serde(default)]
    prompt_tokens_details: Option<PromptTokensDetails>,
}

#[derive(Deserialize)]
struct PromptTokensDetails {
    #[serde(default)]
    cached_tokens: Option<u64>,
}

// ── Provider trait ──────────────────────────────────────────────

#[async_trait]
impl Provider for SeewoProvider {
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            native_tool_calling: true,
            vision: true,
            prompt_caching: false,
        }
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    fn supports_streaming_tool_events(&self) -> bool {
        true
    }

    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<String> {
        let token = self.ensure_token().await?;

        let mut messages = Vec::new();
        if let Some(sys) = system_prompt {
            messages.push(serde_json::json!({"role": "system", "content": sys}));
        }
        messages.push(serde_json::json!({"role": "user", "content": message}));

        let body = Self::build_request_body(&messages, None, temperature);
        let url = Self::build_chat_url(CHAT_BASE_URL, model);

        let resp = self
            .client
            .post(&url)
            .header("x-open-appId", APP_ID)
            .header("x-open-accessToken", &token)
            .header("x-open-userId", USER_ID)
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(super::api_error("Seewo", resp).await);
        }

        let raw: NativeChatResponse = resp.json().await?;
        raw.choices
            .into_iter()
            .next()
            .and_then(|c| {
                c.message
                    .content
                    .filter(|s| !s.is_empty())
                    .or(c.message.reasoning_content)
            })
            .ok_or_else(|| anyhow::anyhow!("No response from Seewo"))
    }

    async fn chat(
        &self,
        request: ProviderChatRequest<'_>,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<ProviderChatResponse> {
        let token = self.ensure_token().await?;

        let messages = Self::convert_messages(request.messages);
        let tools = Self::convert_tools(request.tools);
        let body = Self::build_request_body(&messages, tools.as_ref(), temperature);
        let url = Self::build_chat_url(CHAT_BASE_URL, model);

        let resp = self
            .client
            .post(&url)
            .header("x-open-appId", APP_ID)
            .header("x-open-accessToken", &token)
            .header("x-open-userId", USER_ID)
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(super::api_error("Seewo", resp).await);
        }

        let raw: NativeChatResponse = resp.json().await?;
        Self::parse_response(raw)
    }

    fn stream_chat(
        &self,
        request: ProviderChatRequest<'_>,
        model: &str,
        temperature: f64,
        options: StreamOptions,
    ) -> stream::BoxStream<'static, StreamResult<StreamEvent>> {
        if !options.enabled {
            return stream::once(async { Ok(StreamEvent::Final) }).boxed();
        }

        let messages = Self::convert_messages(request.messages);
        let tools = Self::convert_tools(request.tools);
        let body = Self::build_request_body(&messages, tools.as_ref(), temperature);
        let url = Self::build_chat_url(STREAM_BASE_URL, model);

        let client = self.client.clone();
        let token_cache = Arc::clone(&self.token_cache);
        let count_tokens = options.count_tokens;

        let (tx, rx) = tokio::sync::mpsc::channel::<StreamResult<StreamEvent>>(100);

        tokio::spawn(async move {
            // Acquire token inside the spawned task.
            let token = {
                let provider = SeewoProvider {
                    client: client.clone(),
                    token_cache: token_cache.clone(),
                };
                match provider.ensure_token().await {
                    Ok(t) => t,
                    Err(e) => {
                        let _ = tx
                            .send(Err(crate::providers::traits::StreamError::Provider(
                                e.to_string(),
                            )))
                            .await;
                        return;
                    }
                }
            };

            let resp = match client
                .post(&url)
                .header("x-open-appId", APP_ID)
                .header("x-open-accessToken", &token)
                .header("x-open-userId", USER_ID)
                .header("Accept", "text/event-stream")
                .json(&body)
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    let _ = tx
                        .send(Err(crate::providers::traits::StreamError::Http(e)))
                        .await;
                    return;
                }
            };

            if !resp.status().is_success() {
                let status = resp.status();
                let text = resp.text().await.unwrap_or_default();
                let _ = tx
                    .send(Err(crate::providers::traits::StreamError::Provider(
                        format!("Seewo API error ({status}): {text}"),
                    )))
                    .await;
                return;
            }

            let mut event_stream = sse_bytes_to_events(resp, count_tokens);
            while let Some(event) = event_stream.next().await {
                if tx.send(event).await.is_err() {
                    break;
                }
            }
        });

        stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|event| (event, rx))
        })
        .boxed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compute_sign_is_deterministic() {
        let ts: u64 = 1_775_479_013_017;
        let sign = SeewoProvider::compute_sign(ts);
        assert_eq!(sign, "a79d08c5fff602e246a3b4c1916625db");
    }

    #[test]
    fn convert_messages_plain_text() {
        let messages = vec![
            ChatMessage::system("be helpful"),
            ChatMessage::user("hello"),
        ];
        let converted = SeewoProvider::convert_messages(&messages);
        assert_eq!(converted.len(), 2);
        assert_eq!(converted[0]["role"], "system");
        assert_eq!(converted[1]["content"], "hello");
    }

    #[test]
    fn convert_messages_tool_result() {
        let msg = ChatMessage::tool(r#"{"tool_call_id":"call_abc","content":"done"}"#);
        let converted = SeewoProvider::convert_messages(&[msg]);
        assert_eq!(converted[0]["tool_call_id"], "call_abc");
        assert_eq!(converted[0]["content"], "done");
    }

    #[test]
    fn convert_messages_assistant_tool_calls() {
        let json = serde_json::json!({
            "content": "",
            "tool_calls": [{"id": "c1", "type": "function", "function": {"name": "shell", "arguments": "{}"}}]
        });
        let msg = ChatMessage::assistant(json.to_string());
        let converted = SeewoProvider::convert_messages(&[msg]);
        assert!(converted[0].get("tool_calls").is_some());
        assert_eq!(converted[0]["role"], "assistant");
    }

    #[test]
    fn build_request_body_includes_tools() {
        let messages = vec![serde_json::json!({"role": "user", "content": "hi"})];
        let tools = vec![serde_json::json!({"type": "function", "function": {"name": "test"}})];
        let body = SeewoProvider::build_request_body(&messages, Some(&tools), 0.7);
        assert!(body.get("tools").is_some());
        assert!(body.get("temperature").is_some());
    }

    #[test]
    fn build_request_body_omits_tools_when_none() {
        let messages = vec![serde_json::json!({"role": "user", "content": "hi"})];
        let body = SeewoProvider::build_request_body(&messages, None, 0.5);
        assert!(body.get("tools").is_none());
    }

    #[test]
    fn parse_response_extracts_text() {
        let raw: NativeChatResponse =
            serde_json::from_str(r#"{"choices":[{"message":{"content":"ok"}}]}"#).unwrap();
        let parsed = SeewoProvider::parse_response(raw).unwrap();
        assert_eq!(parsed.text.as_deref(), Some("ok"));
        assert!(parsed.tool_calls.is_empty());
    }

    #[test]
    fn parse_response_extracts_tool_calls() {
        let raw: NativeChatResponse = serde_json::from_str(
            r#"{"choices":[{"message":{"content":"","tool_calls":[{"id":"c1","function":{"name":"weather","arguments":"{}"}}]}}]}"#,
        )
        .unwrap();
        let parsed = SeewoProvider::parse_response(raw).unwrap();
        assert_eq!(parsed.tool_calls.len(), 1);
        assert_eq!(parsed.tool_calls[0].name, "weather");
    }

    #[test]
    fn parse_response_usage() {
        let raw: NativeChatResponse = serde_json::from_str(
            r#"{"choices":[{"message":{"content":"ok"}}],"usage":{"prompt_tokens":10,"completion_tokens":5}}"#,
        )
        .unwrap();
        let parsed = SeewoProvider::parse_response(raw).unwrap();
        let usage = parsed.usage.unwrap();
        assert_eq!(usage.input_tokens, Some(10));
        assert_eq!(usage.output_tokens, Some(5));
    }

    #[test]
    fn parse_response_reasoning_fallback() {
        let raw: NativeChatResponse = serde_json::from_str(
            r#"{"choices":[{"message":{"content":"","reasoning_content":"thinking..."}}]}"#,
        )
        .unwrap();
        let parsed = SeewoProvider::parse_response(raw).unwrap();
        assert_eq!(parsed.text.as_deref(), Some("thinking..."));
        assert_eq!(parsed.reasoning_content.as_deref(), Some("thinking..."));
    }

    #[test]
    fn cached_token_expiry() {
        let token = CachedToken {
            access_token: "test".to_string(),
            expires_at: Instant::now() + std::time::Duration::from_secs(3600),
        };
        assert!(!token.is_expired());

        let expired = CachedToken {
            access_token: "test".to_string(),
            expires_at: Instant::now()
                .checked_sub(std::time::Duration::from_secs(1))
                .unwrap_or(Instant::now()),
        };
        // Note: Instant subtraction may saturate to epoch on some platforms,
        // but the token should still appear expired or at most equal to now.
        assert!(expired.expires_at <= Instant::now());
    }
}
