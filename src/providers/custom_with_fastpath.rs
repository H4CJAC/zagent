//! Custom provider with a regex fast-path in front of an
//! [`OpenAiCompatibleProvider`].
//!
//! Incoming user messages are tested against a set of JSON-configured rules.
//! A match short-circuits the LLM call by returning precomputed assistant
//! text and/or native tool calls, saving one model round-trip on well-known
//! intents. The wrapped tools are still executed by the agent loop — this is
//! a fast *path*, not a mock.
//!
//! Unmatched requests, and any turn whose last message is not a `user`
//! message (e.g. when `tool_result` is being fed back), pass through to the
//! inner provider unchanged.
//!
//! Rules file resolution (highest priority first):
//!   1. the `rules_path` argument to [`CustomWithFastpathProvider::wrap`]
//!      (the provider factory wires this from `runtime.fast_path_rules_path`
//!      in `seewo.toml`);
//!   2. the `CCLAWCORE_FAST_PATH_RULES` environment variable;
//!   3. `./fast-path-rules.json` in the current working directory.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use futures_util::stream::{self, BoxStream, StreamExt};
use regex::Regex;
use serde::Deserialize;

use super::compatible::OpenAiCompatibleProvider;
use super::traits::{
    ChatMessage, ChatRequest, ChatResponse, Provider, ProviderCapabilities, StreamChunk,
    StreamEvent, StreamOptions, StreamResult, ToolCall, ToolsPayload,
};
use crate::tools::ToolSpec;

/// Default rules file name looked up in the current working directory.
const DEFAULT_RULES_PATH: &str = "fast-path-rules.json";
/// Environment variable overriding the rules file path.
const RULES_ENV: &str = "CCLAWCORE_FAST_PATH_RULES";

pub struct CustomWithFastpathProvider {
    inner: OpenAiCompatibleProvider,
    rules: Arc<[FastpathRule]>,
}

/// A compiled fast-path rule.
#[derive(Debug)]
pub struct FastpathRule {
    /// Optional human-readable rule name (used for logging and tool_call_id).
    pub name: Option<String>,
    /// Regex tested against the last user message's content.
    pub pattern: Regex,
    /// Optional assistant text to return. At least one of `response` or
    /// `tool_calls` must be non-empty.
    pub response: Option<String>,
    /// Optional native tool calls to return.
    pub tool_calls: Vec<FastpathToolCall>,
}

#[derive(Debug, Clone)]
pub struct FastpathToolCall {
    pub name: String,
    pub arguments: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct RuleSpec {
    #[serde(default)]
    name: Option<String>,
    pattern: String,
    #[serde(default)]
    response: Option<String>,
    #[serde(default)]
    tool_calls: Vec<ToolCallSpec>,
}

#[derive(Debug, Deserialize)]
struct ToolCallSpec {
    name: String,
    #[serde(default = "empty_args")]
    arguments: serde_json::Value,
}

fn empty_args() -> serde_json::Value {
    serde_json::Value::Object(serde_json::Map::new())
}

impl CustomWithFastpathProvider {
    /// Wrap an [`OpenAiCompatibleProvider`]. Rules are loaded from:
    ///   1. the `rules_path` argument, if `Some` (the provider factory wires
    ///      this from `runtime.fast_path_rules_path` in `seewo.toml`);
    ///   2. the `CCLAWCORE_FAST_PATH_RULES` environment variable;
    ///   3. `./fast-path-rules.json` in the current working directory.
    ///
    /// Any failure (missing file, invalid JSON, invalid regex, empty rule)
    /// is logged as a warning and ignored; the wrapper remains transparent.
    pub fn wrap(inner: OpenAiCompatibleProvider, rules_path: Option<&str>) -> Self {
        let rules = Self::load_rules(rules_path);
        if !rules.is_empty() {
            tracing::info!(
                count = rules.len(),
                "Fast-path provider loaded with active rules"
            );
        }
        Self {
            inner,
            rules: Arc::from(rules.into_boxed_slice()),
        }
    }

    fn load_rules(explicit_path: Option<&str>) -> Vec<FastpathRule> {
        let path = explicit_path
            .map(ToString::to_string)
            .or_else(|| std::env::var(RULES_ENV).ok())
            .unwrap_or_else(|| DEFAULT_RULES_PATH.to_string());

        let path = Path::new(&path);
        if !path.exists() {
            // Quiet by design: fast-path is opt-in, absence is normal.
            return Vec::new();
        }

        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(
                    "Fast-path rules file {} could not be read: {e}; disabling fast path",
                    path.display()
                );
                return Vec::new();
            }
        };

        let specs: Vec<RuleSpec> = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    "Fast-path rules file {} has invalid JSON: {e}; disabling fast path",
                    path.display()
                );
                return Vec::new();
            }
        };

        specs
            .into_iter()
            .filter_map(|spec| match compile_rule(spec) {
                Ok(rule) => Some(rule),
                Err(e) => {
                    tracing::warn!("Fast-path rule skipped: {e}");
                    None
                }
            })
            .collect()
    }

    /// Test the request's last message against the configured rules.
    ///
    /// Only `role == "user"` messages can trigger a match — this guarantees
    /// we never interrupt the second hop of an agent turn (where the last
    /// message is a tool result).
    fn match_rule(&self, messages: &[ChatMessage]) -> Option<&FastpathRule> {
        let last = messages.last()?;
        if last.role != "user" {
            return None;
        }
        let content = last.content.as_str();
        self.rules.iter().find(|r| r.pattern.is_match(content))
    }

    fn build_response(rule: &FastpathRule) -> ChatResponse {
        let tool_calls = rule
            .tool_calls
            .iter()
            .enumerate()
            .map(|(i, tc)| ToolCall {
                id: fastpath_tool_call_id(rule.name.as_deref(), i),
                name: tc.name.clone(),
                arguments: tc.arguments.to_string(),
            })
            .collect();
        ChatResponse {
            text: rule.response.clone(),
            tool_calls,
            usage: None,
            reasoning_content: None,
        }
    }

    fn build_stream_events(rule: &FastpathRule) -> Vec<StreamResult<StreamEvent>> {
        let mut events: Vec<StreamResult<StreamEvent>> = Vec::new();
        if let Some(text) = rule.response.as_deref() {
            if !text.is_empty() {
                events.push(Ok(StreamEvent::TextDelta(StreamChunk::delta(text))));
            }
        }
        for (i, tc) in rule.tool_calls.iter().enumerate() {
            events.push(Ok(StreamEvent::ToolCall(ToolCall {
                id: fastpath_tool_call_id(rule.name.as_deref(), i),
                name: tc.name.clone(),
                arguments: tc.arguments.to_string(),
            })));
        }
        events.push(Ok(StreamEvent::Final));
        events
    }
}

fn compile_rule(spec: RuleSpec) -> Result<FastpathRule, String> {
    let has_response = spec.response.as_deref().is_some_and(|s| !s.is_empty());
    let has_tool_calls = !spec.tool_calls.is_empty();
    if !has_response && !has_tool_calls {
        return Err(format!(
            "rule '{}' has neither response nor tool_calls",
            spec.name.as_deref().unwrap_or("<unnamed>")
        ));
    }
    let pattern = Regex::new(&spec.pattern).map_err(|e| {
        format!(
            "rule '{}' has invalid pattern: {e}",
            spec.name.as_deref().unwrap_or("<unnamed>")
        )
    })?;
    let tool_calls = spec
        .tool_calls
        .into_iter()
        .map(|tc| FastpathToolCall {
            name: tc.name,
            arguments: tc.arguments,
        })
        .collect();
    Ok(FastpathRule {
        name: spec.name,
        pattern,
        response: spec.response,
        tool_calls,
    })
}

/// Generate a stable, OpenAI-schema-compatible tool_call_id of the form
/// `fp-<sanitized_rule_name>-<index>`. The sanitizer keeps ASCII
/// alphanumerics, `-`, and `_`, replacing everything else with `_`.
fn fastpath_tool_call_id(rule_name: Option<&str>, index: usize) -> String {
    let mut sanitized = String::new();
    for ch in rule_name.unwrap_or("rule").chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            sanitized.push(ch);
        } else {
            sanitized.push('_');
        }
    }
    if sanitized.is_empty() {
        sanitized.push_str("rule");
    }
    format!("fp-{sanitized}-{index}")
}

#[async_trait]
impl Provider for CustomWithFastpathProvider {
    fn capabilities(&self) -> ProviderCapabilities {
        self.inner.capabilities()
    }

    fn convert_tools(&self, tools: &[ToolSpec]) -> ToolsPayload {
        self.inner.convert_tools(tools)
    }

    async fn chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<String> {
        self.inner
            .chat_with_system(system_prompt, message, model, temperature)
            .await
    }

    async fn chat_with_history(
        &self,
        messages: &[ChatMessage],
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<String> {
        self.inner
            .chat_with_history(messages, model, temperature)
            .await
    }

    async fn chat_with_tools(
        &self,
        messages: &[ChatMessage],
        tools: &[serde_json::Value],
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<ChatResponse> {
        self.inner
            .chat_with_tools(messages, tools, model, temperature)
            .await
    }

    async fn chat(
        &self,
        request: ChatRequest<'_>,
        model: &str,
        temperature: f64,
    ) -> anyhow::Result<ChatResponse> {
        if let Some(rule) = self.match_rule(request.messages) {
            tracing::debug!(
                rule = rule.name.as_deref().unwrap_or("<unnamed>"),
                "fast-path hit for chat()"
            );
            return Ok(Self::build_response(rule));
        }
        self.inner.chat(request, model, temperature).await
    }

    fn supports_native_tools(&self) -> bool {
        self.inner.supports_native_tools()
    }

    fn supports_streaming(&self) -> bool {
        self.inner.supports_streaming()
    }

    fn supports_streaming_tool_events(&self) -> bool {
        self.inner.supports_streaming_tool_events()
    }

    fn stream_chat(
        &self,
        request: ChatRequest<'_>,
        model: &str,
        temperature: f64,
        options: StreamOptions,
    ) -> BoxStream<'static, StreamResult<StreamEvent>> {
        if let Some(rule) = self.match_rule(request.messages) {
            tracing::debug!(
                rule = rule.name.as_deref().unwrap_or("<unnamed>"),
                "fast-path hit for stream_chat()"
            );
            let events = Self::build_stream_events(rule);
            return stream::iter(events).boxed();
        }
        self.inner.stream_chat(request, model, temperature, options)
    }

    fn stream_chat_with_system(
        &self,
        system_prompt: Option<&str>,
        message: &str,
        model: &str,
        temperature: f64,
        options: StreamOptions,
    ) -> BoxStream<'static, StreamResult<StreamChunk>> {
        self.inner
            .stream_chat_with_system(system_prompt, message, model, temperature, options)
    }

    fn stream_chat_with_history(
        &self,
        messages: &[ChatMessage],
        model: &str,
        temperature: f64,
        options: StreamOptions,
    ) -> BoxStream<'static, StreamResult<StreamChunk>> {
        self.inner
            .stream_chat_with_history(messages, model, temperature, options)
    }

    async fn warmup(&self) -> anyhow::Result<()> {
        self.inner.warmup().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::compatible::AuthStyle;

    fn dummy_inner() -> OpenAiCompatibleProvider {
        OpenAiCompatibleProvider::new_with_vision(
            "test-inner",
            "http://127.0.0.1:0",
            None,
            AuthStyle::Bearer,
            false,
        )
    }

    fn make_rule(
        name: Option<&str>,
        pattern: &str,
        response: Option<&str>,
        tool_calls: Vec<FastpathToolCall>,
    ) -> FastpathRule {
        FastpathRule {
            name: name.map(ToString::to_string),
            pattern: Regex::new(pattern).expect("valid regex"),
            response: response.map(ToString::to_string),
            tool_calls,
        }
    }

    fn provider_with(rules: Vec<FastpathRule>) -> CustomWithFastpathProvider {
        CustomWithFastpathProvider {
            inner: dummy_inner(),
            rules: Arc::from(rules.into_boxed_slice()),
        }
    }

    #[test]
    fn compile_rule_rejects_empty_outputs() {
        let spec = RuleSpec {
            name: Some("empty".into()),
            pattern: ".*".into(),
            response: None,
            tool_calls: vec![],
        };
        assert!(compile_rule(spec).is_err());
    }

    #[test]
    fn compile_rule_rejects_invalid_regex() {
        let spec = RuleSpec {
            name: Some("bad".into()),
            pattern: "(".into(),
            response: Some("x".into()),
            tool_calls: vec![],
        };
        assert!(compile_rule(spec).is_err());
    }

    #[test]
    fn compile_rule_accepts_response_only() {
        let spec = RuleSpec {
            name: Some("text".into()),
            pattern: "hi".into(),
            response: Some("hello".into()),
            tool_calls: vec![],
        };
        let rule = compile_rule(spec).expect("compiles");
        assert_eq!(rule.response.as_deref(), Some("hello"));
        assert!(rule.tool_calls.is_empty());
    }

    #[test]
    fn compile_rule_accepts_tool_calls_only() {
        let spec = RuleSpec {
            name: Some("t".into()),
            pattern: "go".into(),
            response: None,
            tool_calls: vec![ToolCallSpec {
                name: "sw_do_it".into(),
                arguments: serde_json::json!({"x": 1}),
            }],
        };
        let rule = compile_rule(spec).expect("compiles");
        assert_eq!(rule.tool_calls.len(), 1);
    }

    #[test]
    fn match_rule_returns_none_when_last_is_not_user() {
        let provider = provider_with(vec![make_rule(
            Some("hi"),
            "hello",
            Some("hi there"),
            vec![],
        )]);
        let msgs = vec![
            ChatMessage::user("hello"),
            ChatMessage::assistant("hello back"),
        ];
        assert!(provider.match_rule(&msgs).is_none());
    }

    #[test]
    fn match_rule_returns_none_on_empty_messages() {
        let provider = provider_with(vec![make_rule(
            Some("hi"),
            "hello",
            Some("hi there"),
            vec![],
        )]);
        assert!(provider.match_rule(&[]).is_none());
    }

    #[test]
    fn match_rule_hits_on_user_last() {
        let provider = provider_with(vec![make_rule(
            Some("hi"),
            "(?i)hello",
            Some("hi there"),
            vec![],
        )]);
        let msgs = vec![ChatMessage::user("Hello, world")];
        let rule = provider.match_rule(&msgs).expect("should match");
        assert_eq!(rule.name.as_deref(), Some("hi"));
    }

    #[test]
    fn match_rule_first_rule_wins() {
        let provider = provider_with(vec![
            make_rule(Some("a"), "foo", Some("A"), vec![]),
            make_rule(Some("b"), "foo", Some("B"), vec![]),
        ]);
        let msgs = vec![ChatMessage::user("foo bar")];
        let rule = provider.match_rule(&msgs).expect("should match");
        assert_eq!(rule.name.as_deref(), Some("a"));
    }

    #[test]
    fn build_response_text_only() {
        let rule = make_rule(Some("r"), ".*", Some("answer"), vec![]);
        let resp = CustomWithFastpathProvider::build_response(&rule);
        assert_eq!(resp.text.as_deref(), Some("answer"));
        assert!(resp.tool_calls.is_empty());
        assert!(resp.usage.is_none());
    }

    #[test]
    fn build_response_tool_calls_only() {
        let rule = make_rule(
            Some("r"),
            ".*",
            None,
            vec![FastpathToolCall {
                name: "sw_get_user_info".into(),
                arguments: serde_json::json!({}),
            }],
        );
        let resp = CustomWithFastpathProvider::build_response(&rule);
        assert!(resp.text.is_none());
        assert_eq!(resp.tool_calls.len(), 1);
        assert_eq!(resp.tool_calls[0].name, "sw_get_user_info");
        assert_eq!(resp.tool_calls[0].id, "fp-r-0");
        assert_eq!(resp.tool_calls[0].arguments, "{}");
    }

    #[test]
    fn build_response_both_text_and_tool_calls() {
        let rule = make_rule(
            Some("combo"),
            ".*",
            Some("working on it"),
            vec![FastpathToolCall {
                name: "t1".into(),
                arguments: serde_json::json!({"k": "v"}),
            }],
        );
        let resp = CustomWithFastpathProvider::build_response(&rule);
        assert_eq!(resp.text.as_deref(), Some("working on it"));
        assert_eq!(resp.tool_calls.len(), 1);
        let args: serde_json::Value = serde_json::from_str(&resp.tool_calls[0].arguments).unwrap();
        assert_eq!(args, serde_json::json!({"k": "v"}));
    }

    #[test]
    fn build_stream_events_text_and_tool_calls_ends_with_final() {
        let rule = make_rule(
            Some("r"),
            ".*",
            Some("hello"),
            vec![
                FastpathToolCall {
                    name: "t1".into(),
                    arguments: serde_json::json!({}),
                },
                FastpathToolCall {
                    name: "t2".into(),
                    arguments: serde_json::json!({"a": 1}),
                },
            ],
        );
        let events = CustomWithFastpathProvider::build_stream_events(&rule);
        assert_eq!(events.len(), 4);
        match events[0].as_ref().expect("ok") {
            StreamEvent::TextDelta(chunk) => {
                assert_eq!(chunk.delta, "hello");
                assert!(!chunk.is_final);
            }
            other => panic!("expected TextDelta, got {other:?}"),
        }
        match events[1].as_ref().expect("ok") {
            StreamEvent::ToolCall(tc) => {
                assert_eq!(tc.id, "fp-r-0");
                assert_eq!(tc.name, "t1");
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }
        match events[2].as_ref().expect("ok") {
            StreamEvent::ToolCall(tc) => {
                assert_eq!(tc.id, "fp-r-1");
                assert_eq!(tc.name, "t2");
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }
        matches!(events[3].as_ref().expect("ok"), StreamEvent::Final);
    }

    #[test]
    fn build_stream_events_skips_empty_response() {
        let rule = make_rule(
            Some("r"),
            ".*",
            Some(""),
            vec![FastpathToolCall {
                name: "t1".into(),
                arguments: serde_json::json!({}),
            }],
        );
        let events = CustomWithFastpathProvider::build_stream_events(&rule);
        // [ToolCall, Final]
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn fastpath_tool_call_id_sanitizes_chinese() {
        // Each non-ASCII-alphanumeric, non-[-_] char becomes a single `_`:
        // "备课-1 示例" → "__-1___" → "fp-__-1___-3"
        let id = fastpath_tool_call_id(Some("备课-1 示例"), 3);
        assert_eq!(id, "fp-__-1___-3");
    }

    #[test]
    fn fastpath_tool_call_id_handles_empty_name() {
        let id = fastpath_tool_call_id(None, 0);
        assert_eq!(id, "fp-rule-0");
    }

    #[test]
    fn load_rules_returns_empty_when_file_missing() {
        let missing = "/nonexistent/path/fast-path-rules-abc.json";
        let rules = CustomWithFastpathProvider::load_rules(Some(missing));
        assert!(rules.is_empty());
    }

    #[test]
    fn load_rules_returns_empty_on_invalid_json() {
        let tmp = std::env::temp_dir().join("fast-path-invalid.json");
        std::fs::write(&tmp, "not-json").unwrap();
        let rules = CustomWithFastpathProvider::load_rules(tmp.to_str());
        let _ = std::fs::remove_file(&tmp);
        assert!(rules.is_empty());
    }

    #[test]
    fn load_rules_explicit_path_overrides_env() {
        let explicit = std::env::temp_dir().join("fast-path-explicit.json");
        let from_env = std::env::temp_dir().join("fast-path-from-env.json");
        std::fs::write(
            &explicit,
            serde_json::json!([
                {"name": "from-explicit", "pattern": "^x$", "response": "x"}
            ])
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            &from_env,
            serde_json::json!([
                {"name": "from-env", "pattern": "^y$", "response": "y"}
            ])
            .to_string(),
        )
        .unwrap();

        let prev = std::env::var(RULES_ENV).ok();
        unsafe { std::env::set_var(RULES_ENV, from_env.to_str().unwrap()) };

        let rules = CustomWithFastpathProvider::load_rules(explicit.to_str());

        match prev {
            Some(v) => unsafe { std::env::set_var(RULES_ENV, v) },
            None => unsafe { std::env::remove_var(RULES_ENV) },
        }
        let _ = std::fs::remove_file(&explicit);
        let _ = std::fs::remove_file(&from_env);

        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].name.as_deref(), Some("from-explicit"));
    }

    #[test]
    fn load_rules_filters_invalid_entries() {
        let tmp = std::env::temp_dir().join("fast-path-mixed.json");
        let payload = serde_json::json!([
            {"name": "good", "pattern": "(?i)hello", "response": "hi"},
            {"name": "bad-regex", "pattern": "(", "response": "x"},
            {"name": "empty", "pattern": ".*"}
        ]);
        std::fs::write(&tmp, payload.to_string()).unwrap();
        let rules = CustomWithFastpathProvider::load_rules(tmp.to_str());
        let _ = std::fs::remove_file(&tmp);
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].name.as_deref(), Some("good"));
    }

    #[tokio::test]
    async fn chat_returns_mock_on_hit() {
        let provider = provider_with(vec![make_rule(
            Some("greeting"),
            "(?i)^hello",
            Some("hi there"),
            vec![],
        )]);
        let msgs = vec![ChatMessage::user("hello, friend")];
        let request = ChatRequest {
            messages: &msgs,
            tools: None,
        };
        let resp = provider.chat(request, "ignored-model", 0.0).await.unwrap();
        assert_eq!(resp.text.as_deref(), Some("hi there"));
        assert!(resp.tool_calls.is_empty());
    }

    #[tokio::test]
    async fn stream_chat_emits_expected_sequence_on_hit() {
        let provider = provider_with(vec![make_rule(
            Some("greet"),
            "(?i)^hi",
            Some("hello"),
            vec![FastpathToolCall {
                name: "sw_get_user_info".into(),
                arguments: serde_json::json!({}),
            }],
        )]);
        let msgs = vec![ChatMessage::user("hi there")];
        let request = ChatRequest {
            messages: &msgs,
            tools: None,
        };
        let stream = provider.stream_chat(request, "model", 0.0, StreamOptions::new(true));
        let events: Vec<StreamResult<StreamEvent>> = stream.collect().await;
        assert_eq!(events.len(), 3);
        matches!(events[0].as_ref().expect("ok"), StreamEvent::TextDelta(_));
        matches!(events[1].as_ref().expect("ok"), StreamEvent::ToolCall(_));
        matches!(events[2].as_ref().expect("ok"), StreamEvent::Final);
    }
}
