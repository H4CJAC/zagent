//! Custom provider with a regex fast-path in front of an
//! [`OpenAiCompatibleProvider`].
//!
//! Incoming user messages are tested against a set of JSON-configured rules.
//! A match short-circuits the LLM call by returning precomputed assistant
//! text and/or native tool calls, saving one model round-trip on well-known
//! intents. The wrapped tools are still executed by the agent loop — this is
//! a fast *path*, not a mock.
//!
//! # Direct response (default)
//!
//! By default (`direct_response: true`), when the agent loop feeds back
//! tool results from a fast-path rule, the provider intercepts the second
//! call and returns the tool output as a plain-text final answer — saving
//! **two** model round-trips total. The provider recognises its own results
//! via the `fpd-` prefix on `tool_call_id`. Set `"direct_response": false`
//! on a rule if the tool output still needs LLM post-processing.
//!
//! Unmatched requests, and any turn whose last message is not a `user`
//! message (e.g. when `tool_result` is being fed back), pass through to the
//! inner provider unchanged — unless the trailing tool results carry the
//! `fpd-` direct-response marker.
//!
//! Rules file resolution (highest priority first):
//!   1. the `rules_path` argument to [`CustomWithFastpathProvider::wrap`]
//!      (the provider factory wires this from `runtime.fast_path_rules_path`
//!      in `seewo.toml`);
//!   2. the `CCLAWCORE_FAST_PATH_RULES` environment variable;
//!   3. `fast-path-rules.json` in the workspace root.
//!
//! Relative paths from any of the three sources are resolved against
//! `workspace_dir` (mirrors `demo_scripts_dir` in `DemoScriptEngine::load`);
//! absolute paths are used as-is. When `workspace_dir` is `None`, relative
//! paths fall back to the current working directory.
//!
//! # JSON file layout
//!
//! The rules file is an object (top-level arrays are rejected):
//!
//! ```json
//! {
//!   "anchor_prefix": "...optional...",
//!   "anchor_suffix": "...optional...",
//!   "rules": [
//!     { "name": "volume_up", "pattern": "(调大音量|音量大一点)", "tool_calls": [...] },
//!     { "name": "greeting",  "pattern": "^(?i)(你好|hi)[。!！.]?$",
//!       "anchored": false, "response": "..." }
//!   ]
//! }
//! ```
//!
//! # Anchoring
//!
//! Each rule's `pattern` is compiled as
//! `{anchor_prefix}{pattern}{anchor_suffix}` by default, so rules match
//! whole-utterance commands rather than arbitrary substrings. Missing
//! `anchor_prefix` / `anchor_suffix` fall back to the built-in defaults
//! ([`DEFAULT_ANCHOR_PREFIX`] / [`DEFAULT_ANCHOR_SUFFIX`]).
//!
//! Set `"anchored": false` on a specific rule to skip wrapping and use its
//! pattern verbatim — useful when the rule already owns its own `^...$`
//! anchoring or intentionally wants substring semantics.
//!
//! # Implicit negation protection
//!
//! Rust `regex` has no lookaround, so negation protection is encoded via
//! the whitelist tightness of the default anchors:
//!
//! - The prefix only consumes whitespace / Chinese punctuation / an optional
//!   polite opener (`请/麻烦/帮我/...`). Any negation word
//!   (`不要/别/取消/不用/不想/先不/...`) fails to fit this whitelist, so the
//!   overall anchored regex cannot consume characters before the command
//!   phrase → no match.
//! - The suffix only consumes whitespace / a small set of trailing tone
//!   characters (`一下/呗/啊/吧`). Again, `不/别/取消` are absent, so
//!   `"调大音量不行"`-style inputs cannot match.
//!
//! Rules with `anchored: false` opt out of this protection and own their
//! own semantics.

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

/// Default opening fragment wrapped around each anchored rule pattern.
///
/// Allows leading whitespace / Chinese punctuation and zero or more
/// chained polite openers (so "请帮我" works as well as "请" alone), then
/// opens a non-capturing group that the rule pattern is spliced into.
/// Any negation / non-polite token before the command phrase fails this
/// whitelist and causes the rule to not match.
pub const DEFAULT_ANCHOR_PREFIX: &str =
    r"^[\s，,。!！?？]*(?:(?:请|麻烦|帮我|帮忙|能不能|可以|可否|请问|给我|让|来|我想)\s*)*(?:";

/// Default closing fragment wrapped around each anchored rule pattern.
///
/// Closes the non-capturing group opened by [`DEFAULT_ANCHOR_PREFIX`] and
/// tolerates trailing whitespace / punctuation / a small set of Chinese
/// tone characters. Negation characters are intentionally absent from the
/// trailing whitelist.
pub const DEFAULT_ANCHOR_SUFFIX: &str = r")\s*[\s，,。!！?？一下呗啊吧]*$";

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
    /// When true (the default), the provider intercepts the second-hop call
    /// after tool execution and returns the tool output directly as the final
    /// response, skipping the follow-up LLM call. Tool-call IDs use the
    /// `fpd-` prefix so the provider can recognise its own results.
    pub direct_response: bool,
}

#[derive(Debug, Clone)]
pub struct FastpathToolCall {
    pub name: String,
    pub arguments: serde_json::Value,
}

/// Top-level schema for the fast-path rules JSON file. Only the object
/// form is accepted; legacy top-level arrays are treated as invalid JSON.
#[derive(Debug, Default, Deserialize)]
struct RulesFileSpec {
    #[serde(default)]
    anchor_prefix: Option<String>,
    #[serde(default)]
    anchor_suffix: Option<String>,
    #[serde(default)]
    rules: Vec<RuleSpec>,
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
    /// When `None` or `Some(true)`, the rule's pattern is wrapped by the
    /// bundle's `anchor_prefix` / `anchor_suffix` (or the built-in
    /// defaults). When `Some(false)`, the pattern is used verbatim — the
    /// rule author takes full responsibility for anchoring.
    #[serde(default)]
    anchored: Option<bool>,
    /// When `None` or `Some(true)`, tool results from this rule are returned
    /// directly to the user without an additional LLM round-trip. Set to
    /// `Some(false)` if the tool output needs LLM post-processing.
    #[serde(default)]
    direct_response: Option<bool>,
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

/// Resolve a rules path string:
/// - absolute paths are used as-is;
/// - relative paths join `workspace_dir` when provided, else CWD-relative.
/// Mirrors the semantics used by `DemoScriptEngine::load` for
/// `demo_scripts_dir`.
fn resolve_rules_path(workspace_dir: Option<&Path>, raw: &str) -> std::path::PathBuf {
    let p = Path::new(raw);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        match workspace_dir {
            Some(root) => root.join(p),
            None => p.to_path_buf(),
        }
    }
}

impl CustomWithFastpathProvider {
    /// Wrap an [`OpenAiCompatibleProvider`]. Rules are loaded from:
    ///   1. the `rules_path` argument, if `Some` (the provider factory wires
    ///      this from `runtime.fast_path_rules_path` in `seewo.toml`);
    ///   2. the `CCLAWCORE_FAST_PATH_RULES` environment variable;
    ///   3. `fast-path-rules.json` in the workspace root.
    ///
    /// The rules file must deserialize as an object with shape
    /// `{ "anchor_prefix"?: string, "anchor_suffix"?: string, "rules": [...] }`.
    /// Top-level arrays are rejected (treated as invalid JSON).
    ///
    /// Each rule's pattern is wrapped by
    /// `anchor_prefix + pattern + anchor_suffix` unless the rule sets
    /// `"anchored": false`. When the bundle omits `anchor_prefix` /
    /// `anchor_suffix`, [`DEFAULT_ANCHOR_PREFIX`] and
    /// [`DEFAULT_ANCHOR_SUFFIX`] are used.
    ///
    /// Relative paths from any source are resolved against `workspace_dir`
    /// (absolute paths are used as-is). When `workspace_dir` is `None`,
    /// relative paths fall back to the process working directory.
    ///
    /// Any failure (missing file, invalid JSON, invalid regex, empty rule)
    /// is logged as a warning and ignored; the wrapper remains transparent.
    pub fn wrap(
        inner: OpenAiCompatibleProvider,
        workspace_dir: Option<&Path>,
        rules_path: Option<&str>,
    ) -> Self {
        let rules = Self::load_rules(workspace_dir, rules_path);
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

    fn load_rules(workspace_dir: Option<&Path>, explicit_path: Option<&str>) -> Vec<FastpathRule> {
        let raw = explicit_path
            .map(ToString::to_string)
            .or_else(|| std::env::var(RULES_ENV).ok())
            .unwrap_or_else(|| DEFAULT_RULES_PATH.to_string());

        let path_buf = resolve_rules_path(workspace_dir, &raw);
        let path = path_buf.as_path();
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

        let bundle: RulesFileSpec = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    "Fast-path rules file {} has invalid JSON: {e}; disabling fast path",
                    path.display()
                );
                return Vec::new();
            }
        };

        let prefix = bundle
            .anchor_prefix
            .as_deref()
            .unwrap_or(DEFAULT_ANCHOR_PREFIX);
        let suffix = bundle
            .anchor_suffix
            .as_deref()
            .unwrap_or(DEFAULT_ANCHOR_SUFFIX);

        bundle
            .rules
            .into_iter()
            .filter_map(|spec| match compile_rule(spec, prefix, suffix) {
                Ok(rule) => Some(rule),
                Err(e) => {
                    tracing::warn!("Fast-path rule skipped: {e}");
                    None
                }
            })
            .collect()
    }

    /// Check whether the trailing messages are tool results from a
    /// direct-response fast-path rule (identified by the `fpd-` prefix on
    /// `tool_call_id`). When they are, the tool output is returned as a
    /// plain text response so the agent loop treats it as the final answer
    /// without an extra LLM round-trip.
    fn extract_direct_tool_results(messages: &[ChatMessage]) -> Option<String> {
        let trailing_tools: Vec<&ChatMessage> = messages
            .iter()
            .rev()
            .take_while(|m| m.role == "tool")
            .collect();
        if trailing_tools.is_empty() {
            return None;
        }
        let mut contents = Vec::new();
        for msg in trailing_tools.iter().rev() {
            let parsed: serde_json::Value = serde_json::from_str(msg.content.as_str()).ok()?;
            let id = parsed.get("tool_call_id")?.as_str()?;
            if !id.starts_with(DIRECT_ID_PREFIX) {
                return None;
            }
            if let Some(c) = parsed.get("content").and_then(|v| v.as_str()) {
                contents.push(c.to_string());
            }
        }
        Some(contents.join("\n"))
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
        let direct = rule.direct_response;
        let tool_calls = rule
            .tool_calls
            .iter()
            .enumerate()
            .map(|(i, tc)| ToolCall {
                id: fastpath_tool_call_id(rule.name.as_deref(), i, direct),
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

    /// Debug-print the full LLM request body just before it is forwarded to
    /// the wrapped provider. Only runs when the `DEBUG` level is enabled and
    /// only on the pass-through path (fast-path hits never reach the LLM).
    fn debug_log_request(request: &ChatRequest<'_>, model: &str, temperature: f64) {
        if !tracing::enabled!(tracing::Level::DEBUG) {
            return;
        }
        let body = serde_json::json!({
            "model": model,
            "temperature": temperature,
            "messages": request.messages,
            "tools": request.tools,
        });
        match serde_json::to_string_pretty(&body) {
            Ok(s) => tracing::debug!("custom-with-fastpath LLM request body:\n{}", s),
            Err(e) => {
                tracing::debug!("custom-with-fastpath request body serialization failed: {e}")
            }
        }
    }

    fn build_stream_events(rule: &FastpathRule) -> Vec<StreamResult<StreamEvent>> {
        let direct = rule.direct_response;
        let mut events: Vec<StreamResult<StreamEvent>> = Vec::new();
        if let Some(text) = rule.response.as_deref() {
            if !text.is_empty() {
                events.push(Ok(StreamEvent::TextDelta(StreamChunk::delta(text))));
            }
        }
        for (i, tc) in rule.tool_calls.iter().enumerate() {
            events.push(Ok(StreamEvent::ToolCall(ToolCall {
                id: fastpath_tool_call_id(rule.name.as_deref(), i, direct),
                name: tc.name.clone(),
                arguments: tc.arguments.to_string(),
            })));
        }
        events.push(Ok(StreamEvent::Final));
        events
    }
}

fn compile_rule(spec: RuleSpec, prefix: &str, suffix: &str) -> Result<FastpathRule, String> {
    let has_response = spec.response.as_deref().is_some_and(|s| !s.is_empty());
    let has_tool_calls = !spec.tool_calls.is_empty();
    if !has_response && !has_tool_calls {
        return Err(format!(
            "rule '{}' has neither response nor tool_calls",
            spec.name.as_deref().unwrap_or("<unnamed>")
        ));
    }
    let effective_pattern = if spec.anchored.unwrap_or(true) {
        format!("{prefix}{}{suffix}", spec.pattern)
    } else {
        spec.pattern.clone()
    };
    let pattern = Regex::new(&effective_pattern).map_err(|e| {
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
        direct_response: spec.direct_response.unwrap_or(true),
    })
}

/// Prefix on tool-call IDs for direct-response fast-path rules.
/// The provider recognises this prefix on the second hop to short-circuit
/// the LLM call and return tool output directly.
const DIRECT_ID_PREFIX: &str = "fpd-";

/// Generate a stable, OpenAI-schema-compatible tool_call_id.
///
/// * `direct == true`  → `fpd-<sanitized_rule_name>-<index>`
/// * `direct == false` → `fp-<sanitized_rule_name>-<index>`
///
/// The sanitizer keeps ASCII alphanumerics, `-`, and `_`, replacing
/// everything else with `_`.
fn fastpath_tool_call_id(rule_name: Option<&str>, index: usize, direct: bool) -> String {
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
    let tag = if direct { "fpd" } else { "fp" };
    format!("{tag}-{sanitized}-{index}")
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
        if let Some(direct_text) = Self::extract_direct_tool_results(request.messages) {
            tracing::debug!("fast-path direct response (second hop)");
            return Ok(ChatResponse {
                text: Some(direct_text),
                tool_calls: vec![],
                usage: None,
                reasoning_content: None,
            });
        }
        if let Some(rule) = self.match_rule(request.messages) {
            tracing::debug!(
                rule = rule.name.as_deref().unwrap_or("<unnamed>"),
                "fast-path hit for chat()"
            );
            return Ok(Self::build_response(rule));
        }
        Self::debug_log_request(&request, model, temperature);
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
        if let Some(direct_text) = Self::extract_direct_tool_results(request.messages) {
            tracing::debug!("fast-path direct response (second hop, stream)");
            let events = vec![
                Ok(StreamEvent::TextDelta(StreamChunk::delta(&direct_text))),
                Ok(StreamEvent::Final),
            ];
            return stream::iter(events).boxed();
        }
        if let Some(rule) = self.match_rule(request.messages) {
            tracing::debug!(
                rule = rule.name.as_deref().unwrap_or("<unnamed>"),
                "fast-path hit for stream_chat()"
            );
            let events = Self::build_stream_events(rule);
            return stream::iter(events).boxed();
        }
        Self::debug_log_request(&request, model, temperature);
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
        make_rule_with_direct(name, pattern, response, tool_calls, true)
    }

    fn make_rule_with_direct(
        name: Option<&str>,
        pattern: &str,
        response: Option<&str>,
        tool_calls: Vec<FastpathToolCall>,
        direct_response: bool,
    ) -> FastpathRule {
        FastpathRule {
            name: name.map(ToString::to_string),
            pattern: Regex::new(pattern).expect("valid regex"),
            response: response.map(ToString::to_string),
            tool_calls,
            direct_response,
        }
    }

    fn provider_with(rules: Vec<FastpathRule>) -> CustomWithFastpathProvider {
        CustomWithFastpathProvider {
            inner: dummy_inner(),
            rules: Arc::from(rules.into_boxed_slice()),
        }
    }

    fn compile_with_defaults(spec: RuleSpec) -> Result<FastpathRule, String> {
        compile_rule(spec, DEFAULT_ANCHOR_PREFIX, DEFAULT_ANCHOR_SUFFIX)
    }

    #[test]
    fn compile_rule_rejects_empty_outputs() {
        let spec = RuleSpec {
            name: Some("empty".into()),
            pattern: ".*".into(),
            response: None,
            tool_calls: vec![],
            anchored: None,
            direct_response: None,
        };
        assert!(compile_with_defaults(spec).is_err());
    }

    #[test]
    fn compile_rule_rejects_invalid_regex() {
        let spec = RuleSpec {
            name: Some("bad".into()),
            pattern: "(".into(),
            response: Some("x".into()),
            tool_calls: vec![],
            anchored: None,
            direct_response: None,
        };
        assert!(compile_with_defaults(spec).is_err());
    }

    #[test]
    fn compile_rule_accepts_response_only() {
        let spec = RuleSpec {
            name: Some("text".into()),
            pattern: "(hi)".into(),
            response: Some("hello".into()),
            tool_calls: vec![],
            anchored: None,
            direct_response: None,
        };
        let rule = compile_with_defaults(spec).expect("compiles");
        assert_eq!(rule.response.as_deref(), Some("hello"));
        assert!(rule.tool_calls.is_empty());
        assert!(rule.direct_response, "default should be true");
    }

    #[test]
    fn compile_rule_accepts_tool_calls_only() {
        let spec = RuleSpec {
            name: Some("t".into()),
            pattern: "(go)".into(),
            response: None,
            tool_calls: vec![ToolCallSpec {
                name: "sw_do_it".into(),
                arguments: serde_json::json!({"x": 1}),
            }],
            anchored: None,
            direct_response: None,
        };
        let rule = compile_with_defaults(spec).expect("compiles");
        assert_eq!(rule.tool_calls.len(), 1);
        assert!(rule.direct_response);
    }

    #[test]
    fn compile_rule_direct_response_explicit_false() {
        let spec = RuleSpec {
            name: Some("no-direct".into()),
            pattern: "(go)".into(),
            response: Some("ok".into()),
            tool_calls: vec![],
            anchored: None,
            direct_response: Some(false),
        };
        let rule = compile_with_defaults(spec).expect("compiles");
        assert!(!rule.direct_response);
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
        // Go through compile_rule with anchored:false so we exercise the
        // explicit opt-out path (substring semantics).
        let rule = compile_with_defaults(RuleSpec {
            name: Some("hi".into()),
            pattern: "(?i)hello".into(),
            response: Some("hi there".into()),
            tool_calls: vec![],
            anchored: Some(false),
            direct_response: None,
        })
        .expect("compiles");
        let provider = provider_with(vec![rule]);
        let msgs = vec![ChatMessage::user("Hello, world")];
        let hit = provider.match_rule(&msgs).expect("should match");
        assert_eq!(hit.name.as_deref(), Some("hi"));
    }

    #[test]
    fn match_rule_first_rule_wins() {
        let a = compile_with_defaults(RuleSpec {
            name: Some("a".into()),
            pattern: "foo".into(),
            response: Some("A".into()),
            tool_calls: vec![],
            anchored: Some(false),
            direct_response: None,
        })
        .expect("compiles");
        let b = compile_with_defaults(RuleSpec {
            name: Some("b".into()),
            pattern: "foo".into(),
            response: Some("B".into()),
            tool_calls: vec![],
            anchored: Some(false),
            direct_response: None,
        })
        .expect("compiles");
        let provider = provider_with(vec![a, b]);
        let msgs = vec![ChatMessage::user("foo")];
        let hit = provider.match_rule(&msgs).expect("should match");
        assert_eq!(hit.name.as_deref(), Some("a"));
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
        assert_eq!(resp.tool_calls[0].id, "fpd-r-0");
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
                assert_eq!(tc.id, "fpd-r-0");
                assert_eq!(tc.name, "t1");
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }
        match events[2].as_ref().expect("ok") {
            StreamEvent::ToolCall(tc) => {
                assert_eq!(tc.id, "fpd-r-1");
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
        let id = fastpath_tool_call_id(Some("备课-1 示例"), 3, true);
        assert_eq!(id, "fpd-__-1___-3");
        let id = fastpath_tool_call_id(Some("备课-1 示例"), 3, false);
        assert_eq!(id, "fp-__-1___-3");
    }

    #[test]
    fn fastpath_tool_call_id_handles_empty_name() {
        let id = fastpath_tool_call_id(None, 0, true);
        assert_eq!(id, "fpd-rule-0");
        let id = fastpath_tool_call_id(None, 0, false);
        assert_eq!(id, "fp-rule-0");
    }

    #[test]
    fn load_rules_returns_empty_when_file_missing() {
        let missing = "/nonexistent/path/fast-path-rules-abc.json";
        let rules = CustomWithFastpathProvider::load_rules(None, Some(missing));
        assert!(rules.is_empty());
    }

    #[test]
    fn load_rules_returns_empty_on_invalid_json() {
        let tmp = std::env::temp_dir().join("fast-path-invalid.json");
        std::fs::write(&tmp, "not-json").unwrap();
        let rules = CustomWithFastpathProvider::load_rules(None, tmp.to_str());
        let _ = std::fs::remove_file(&tmp);
        assert!(rules.is_empty());
    }

    #[test]
    fn load_rules_explicit_path_overrides_env() {
        let explicit = std::env::temp_dir().join("fast-path-explicit.json");
        let from_env = std::env::temp_dir().join("fast-path-from-env.json");
        std::fs::write(
            &explicit,
            serde_json::json!({
                "rules": [
                    {"name": "from-explicit", "pattern": "^x$", "response": "x",
                     "anchored": false}
                ]
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            &from_env,
            serde_json::json!({
                "rules": [
                    {"name": "from-env", "pattern": "^y$", "response": "y",
                     "anchored": false}
                ]
            })
            .to_string(),
        )
        .unwrap();

        let prev = std::env::var(RULES_ENV).ok();
        unsafe { std::env::set_var(RULES_ENV, from_env.to_str().unwrap()) };

        let rules = CustomWithFastpathProvider::load_rules(None, explicit.to_str());

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
    fn load_rules_resolves_relative_against_workspace_dir() {
        // Unique workspace root under tmp to avoid collisions across tests.
        let workspace = std::env::temp_dir().join("fast-path-ws-reldir");
        std::fs::create_dir_all(&workspace).unwrap();
        let rules_file = workspace.join("rules.json");
        std::fs::write(
            &rules_file,
            serde_json::json!({
                "rules": [
                    {"name": "ws-hit", "pattern": "^z$", "response": "z",
                     "anchored": false}
                ]
            })
            .to_string(),
        )
        .unwrap();

        // Relative path "rules.json" should resolve under workspace.
        let rules = CustomWithFastpathProvider::load_rules(Some(&workspace), Some("rules.json"));

        let _ = std::fs::remove_file(&rules_file);
        let _ = std::fs::remove_dir(&workspace);

        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].name.as_deref(), Some("ws-hit"));
    }

    #[test]
    fn resolve_rules_path_keeps_absolute() {
        let abs = if cfg!(windows) {
            "C:/tmp/rules.json"
        } else {
            "/tmp/rules.json"
        };
        let ws = Path::new("/tmp/workspace");
        let p = resolve_rules_path(Some(ws), abs);
        assert_eq!(p, Path::new(abs));
    }

    #[test]
    fn resolve_rules_path_relative_without_workspace_keeps_as_is() {
        let p = resolve_rules_path(None, "rules.json");
        assert_eq!(p, Path::new("rules.json"));
    }

    #[test]
    fn load_rules_filters_invalid_entries() {
        let tmp = std::env::temp_dir().join("fast-path-mixed.json");
        let payload = serde_json::json!({
            "rules": [
                {"name": "good", "pattern": "(?i)hello", "response": "hi",
                 "anchored": false},
                {"name": "bad-regex", "pattern": "(", "response": "x"},
                {"name": "empty", "pattern": ".*"}
            ]
        });
        std::fs::write(&tmp, payload.to_string()).unwrap();
        let rules = CustomWithFastpathProvider::load_rules(None, tmp.to_str());
        let _ = std::fs::remove_file(&tmp);
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].name.as_deref(), Some("good"));
    }

    #[test]
    fn load_rules_parses_bundle_with_custom_anchors() {
        let tmp = std::env::temp_dir().join("fast-path-custom-anchors.json");
        let payload = serde_json::json!({
            "anchor_prefix": "^",
            "anchor_suffix": "$",
            "rules": [
                {"name": "literal-foo", "pattern": "foo", "response": "ok"}
            ]
        });
        std::fs::write(&tmp, payload.to_string()).unwrap();
        let rules = CustomWithFastpathProvider::load_rules(None, tmp.to_str());
        let _ = std::fs::remove_file(&tmp);
        assert_eq!(rules.len(), 1);
        let rule = &rules[0];
        assert!(rule.pattern.is_match("foo"), "exact literal should hit");
        assert!(
            !rule.pattern.is_match("foo bar"),
            "trailing text should not hit under custom ^$ anchors"
        );
    }

    #[test]
    fn compile_rule_anchored_rejects_composite_input() {
        let spec = RuleSpec {
            name: Some("volume-up".into()),
            pattern: "(调大音量)".into(),
            response: Some("ok".into()),
            tool_calls: vec![],
            anchored: None, // default = true
            direct_response: None,
        };
        let rule = compile_with_defaults(spec).expect("compiles");
        assert!(
            !rule.pattern.is_match("先调大音量再关摄像头"),
            "composite sentence must not match default-anchored rule"
        );
        assert!(
            rule.pattern.is_match("请帮我调大音量一下"),
            "polite prefix + tone suffix must match under default anchors"
        );
    }

    #[test]
    fn compile_rule_anchored_false_keeps_substring_semantics() {
        let spec = RuleSpec {
            name: Some("volume-up-loose".into()),
            pattern: "(调大音量)".into(),
            response: Some("ok".into()),
            tool_calls: vec![],
            anchored: Some(false),
            direct_response: None,
        };
        let rule = compile_with_defaults(spec).expect("compiles");
        assert!(
            rule.pattern.is_match("先调大音量再关摄像头"),
            "anchored:false escape hatch must keep substring semantics"
        );
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

    // ── direct-response tests ──────────────────────────────────────────

    fn tool_result_msg(tool_call_id: &str, content: &str) -> ChatMessage {
        ChatMessage::tool(
            serde_json::json!({ "tool_call_id": tool_call_id, "content": content }).to_string(),
        )
    }

    #[test]
    fn extract_direct_tool_results_returns_content_for_fpd_prefix() {
        let msgs = vec![
            ChatMessage::user("调大音量"),
            ChatMessage::assistant(""),
            tool_result_msg("fpd-volume_up-0", "音量已调整到60"),
        ];
        let result = CustomWithFastpathProvider::extract_direct_tool_results(&msgs);
        assert_eq!(result.as_deref(), Some("音量已调整到60"));
    }

    #[test]
    fn extract_direct_tool_results_joins_multiple_tool_msgs() {
        let msgs = vec![
            ChatMessage::user("test"),
            ChatMessage::assistant(""),
            tool_result_msg("fpd-rule-0", "result A"),
            tool_result_msg("fpd-rule-1", "result B"),
        ];
        let result = CustomWithFastpathProvider::extract_direct_tool_results(&msgs);
        assert_eq!(result.as_deref(), Some("result A\nresult B"));
    }

    #[test]
    fn extract_direct_tool_results_returns_none_for_fp_prefix() {
        let msgs = vec![
            ChatMessage::user("test"),
            ChatMessage::assistant(""),
            tool_result_msg("fp-rule-0", "some output"),
        ];
        assert!(CustomWithFastpathProvider::extract_direct_tool_results(&msgs).is_none());
    }

    #[test]
    fn extract_direct_tool_results_returns_none_when_last_is_user() {
        let msgs = vec![ChatMessage::user("hello")];
        assert!(CustomWithFastpathProvider::extract_direct_tool_results(&msgs).is_none());
    }

    #[test]
    fn extract_direct_tool_results_returns_none_on_mixed_prefixes() {
        let msgs = vec![
            ChatMessage::assistant(""),
            tool_result_msg("fpd-a-0", "ok"),
            tool_result_msg("fp-b-0", "not direct"),
        ];
        assert!(CustomWithFastpathProvider::extract_direct_tool_results(&msgs).is_none());
    }

    #[tokio::test]
    async fn chat_intercepts_second_hop_with_fpd_tool_results() {
        let provider = provider_with(vec![make_rule(
            Some("vol"),
            "(?i)^调大音量",
            None,
            vec![FastpathToolCall {
                name: "changeVolume".into(),
                arguments: serde_json::json!({"mode": "RELATIVE", "value": 10}),
            }],
        )]);
        let msgs = vec![
            ChatMessage::user("调大音量"),
            ChatMessage::assistant(""),
            tool_result_msg("fpd-vol-0", "音量已调整到60"),
        ];
        let request = ChatRequest {
            messages: &msgs,
            tools: None,
        };
        let resp = provider.chat(request, "model", 0.0).await.unwrap();
        assert_eq!(resp.text.as_deref(), Some("音量已调整到60"));
        assert!(resp.tool_calls.is_empty());
    }

    #[tokio::test]
    async fn chat_does_not_intercept_non_direct_fp_results() {
        let provider = provider_with(vec![make_rule_with_direct(
            Some("vol"),
            "(?i)^调大音量",
            None,
            vec![FastpathToolCall {
                name: "changeVolume".into(),
                arguments: serde_json::json!({}),
            }],
            false,
        )]);
        let resp = CustomWithFastpathProvider::build_response(provider.rules.first().unwrap());
        assert!(
            resp.tool_calls[0].id.starts_with("fp-"),
            "non-direct rule should use fp- prefix"
        );
    }

    #[tokio::test]
    async fn stream_chat_intercepts_second_hop_with_fpd_tool_results() {
        let provider = provider_with(vec![]);
        let msgs = vec![
            ChatMessage::user("调大音量"),
            ChatMessage::assistant(""),
            tool_result_msg("fpd-vol-0", "音量已调整到60"),
        ];
        let request = ChatRequest {
            messages: &msgs,
            tools: None,
        };
        let stream = provider.stream_chat(request, "model", 0.0, StreamOptions::new(true));
        let events: Vec<StreamResult<StreamEvent>> = stream.collect().await;
        assert_eq!(events.len(), 2);
        match events[0].as_ref().expect("ok") {
            StreamEvent::TextDelta(chunk) => assert_eq!(chunk.delta, "音量已调整到60"),
            other => panic!("expected TextDelta, got {other:?}"),
        }
        matches!(events[1].as_ref().expect("ok"), StreamEvent::Final);
    }
}
