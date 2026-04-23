//! `image_read` — delegate image understanding to a configured vision model.
//!
//! The tool receives a workspace path (or optionally an HTTP(S) URL), reads
//! the image, base64-encodes it into a `data:` URI, and issues an isolated
//! `chat_with_system` call through a dedicated provider instance built by
//! the standard `providers::create_provider_with_options` factory. The
//! vision model's textual answer is returned as the tool output, so the
//! *main* agent never has to be multimodal itself.
//!
//! The image is transmitted via the existing `[IMAGE:data:...;base64,...]`
//! marker protocol (see `crate::multimodal`). Provider implementations that
//! honour the marker (OpenAI, Seewo, Ollama …) will unpack it into native
//! multimodal parts automatically — no new wire format is introduced.
//! Providers that do not implement marker translation (currently Anthropic
//! and Gemini) may receive the payload as plain text and fail to see the
//! image — see `docs/setup-guides/vision-setup.md` for details.

use super::traits::{Tool, ToolResult};
use crate::config::VisionConfig;
use crate::providers::{create_provider_with_options, Provider, ProviderRuntimeOptions};
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::Path;
use std::sync::Arc;

/// Allowed MIME types for the vision input. Mirrors
/// `crate::multimodal::ALLOWED_IMAGE_MIME_TYPES`.
const ALLOWED_MIME_TYPES: &[&str] = &[
    "image/png",
    "image/jpeg",
    "image/webp",
    "image/gif",
    "image/bmp",
];

/// Default system prompt when the user hasn't overridden it via
/// `[vision] system_prompt = ...`.
const DEFAULT_SYSTEM_PROMPT: &str = "你是图像内容分析助手。请用客观、简洁的中文描述图片中的关键信息：可见的文字（原样转写）、主要对象、场景、布局、颜色、数据图表中的数值等。不要推测不可见的信息。";

/// Default user question when the caller doesn't supply one.
const DEFAULT_QUESTION: &str = "请详细描述这张图片的内容，包括文字、对象、场景";

/// `image_read` tool — delegates vision to a configured OpenAI-compatible
/// endpoint and returns the model's textual answer.
pub struct ImageReadTool {
    security: Arc<SecurityPolicy>,
    cfg: VisionConfig,
    provider_factory: ProviderFactory,
}

/// Factory for the vision provider. Defaults to delegating to the shared
/// `providers::create_provider_with_options` factory so the tool supports
/// any built-in provider (openai, anthropic, gemini, ollama, seewo, …) or
/// URL-prefixed form (`custom:…`, `anthropic:…`, …). Unit tests override
/// it with a mock provider to avoid network I/O.
type ProviderFactory =
    Arc<dyn Fn(&VisionConfig) -> anyhow::Result<Arc<dyn Provider>> + Send + Sync>;

impl ImageReadTool {
    pub fn new(security: Arc<SecurityPolicy>, cfg: VisionConfig) -> Self {
        Self {
            security,
            cfg,
            provider_factory: Arc::new(default_provider_factory),
        }
    }

    /// Test-only constructor that injects a custom provider factory. The
    /// factory is invoked **inside** `execute` so each call gets a fresh
    /// provider instance (matching production behaviour).
    #[cfg(test)]
    fn with_provider_factory<F>(
        security: Arc<SecurityPolicy>,
        cfg: VisionConfig,
        factory: F,
    ) -> Self
    where
        F: Fn(&VisionConfig) -> anyhow::Result<Arc<dyn Provider>> + Send + Sync + 'static,
    {
        Self {
            security,
            cfg,
            provider_factory: Arc::new(factory),
        }
    }

    async fn run(&self, args: Value) -> ToolResult {
        let inputs = match parse_inputs(&args) {
            Ok(i) => i,
            Err(e) => return err(e),
        };

        // Security: path must be inside the workspace; URL must be enabled.
        match &inputs.source {
            Source::Path(path_str) => {
                if !self.security.is_path_allowed(path_str) {
                    return err(format!(
                        "Path not allowed: {path_str} (must be within workspace)"
                    ));
                }
            }
            Source::Url(url) => {
                if !self.cfg.allow_url {
                    return err(
                        "URL inputs are disabled. Enable [vision].allow_url to use remote images.",
                    );
                }
                if !(url.starts_with("http://") || url.starts_with("https://")) {
                    return err(format!("Unsupported URL scheme: {url}"));
                }
            }
        }

        // Build the data URI payload for the marker.
        let data_uri = match &inputs.source {
            Source::Path(path_str) => {
                match encode_local_image(path_str, self.cfg.max_image_bytes).await {
                    Ok(uri) => uri,
                    Err(e) => return err(e),
                }
            }
            Source::Url(url) => url.clone(), // forwarded as-is; provider layer fetches it.
        };

        // Resolve API key. Literal `[vision].api_key` wins over
        // `api_key_env`; this mirrors the top-level provider `api_key`
        // contract so users can put everything in the config file.
        let api_key = match resolve_api_key(&self.cfg) {
            Ok(v) => v,
            Err(e) => return err(e),
        };

        // Resolve overrides.
        let model = inputs
            .model
            .unwrap_or_else(|| self.cfg.default_model.clone());
        let temperature = inputs.temperature.unwrap_or(self.cfg.default_temperature);
        let system_prompt = self
            .cfg
            .system_prompt
            .clone()
            .unwrap_or_else(|| DEFAULT_SYSTEM_PROMPT.to_string());

        // The default factory reads `cfg.api_key_env` from the process env
        // when constructing the provider; `api_key` is validated here only
        // to fail fast with a clear message before any network I/O.
        let _ = api_key;

        let user_message = compose_user_message(&inputs.question, &data_uri);
        let provider = match (self.provider_factory)(&self.cfg) {
            Ok(p) => p,
            Err(e) => return err(format!("Vision provider init failed: {e}")),
        };

        match provider
            .chat_with_system(Some(&system_prompt), &user_message, &model, temperature)
            .await
        {
            Ok(text) => ToolResult {
                success: true,
                output: text.trim().to_string(),
                error: None,
            },
            Err(e) => err(format!("Vision call failed: {e}")),
        }
    }
}

#[async_trait]
impl Tool for ImageReadTool {
    fn name(&self) -> &str {
        "image_read"
    }

    fn description(&self) -> &str {
        "Read and understand an image (workspace path or URL) by delegating to a configured vision model. Returns a text description. Useful when the main agent is text-only but the user asks about an image."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Workspace-relative or absolute path to the image file. Mutually exclusive with `url`."
                },
                "url": {
                    "type": "string",
                    "description": "HTTP(S) URL of the image. Only allowed when [vision].allow_url is true."
                },
                "question": {
                    "type": "string",
                    "description": "Question or instruction for the vision model (e.g. '请提取图中所有文字'). Optional."
                },
                "model": {
                    "type": "string",
                    "description": "Override the default vision model identifier. Optional."
                },
                "temperature": {
                    "type": "number",
                    "description": "Override [vision].default_temperature for this call (typical range 0.0–1.0). Optional."
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        Ok(self.run(args).await)
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────

#[derive(Debug, PartialEq, Eq)]
enum Source {
    Path(String),
    Url(String),
}

#[derive(Debug)]
struct Inputs {
    source: Source,
    question: String,
    model: Option<String>,
    temperature: Option<f64>,
}

fn parse_inputs(args: &Value) -> Result<Inputs, String> {
    let path = args
        .get("path")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let url = args
        .get("url")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let source = match (path, url) {
        (Some(_), Some(_)) => {
            return Err("Provide exactly one of `path` or `url`, not both".into());
        }
        (None, None) => {
            return Err("Missing `path` or `url`: one image source is required".into());
        }
        (Some(p), None) => Source::Path(p.to_string()),
        (None, Some(u)) => Source::Url(u.to_string()),
    };

    let question = args
        .get("question")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_QUESTION)
        .to_string();

    let model = args
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let temperature = match args.get("temperature") {
        None | Some(Value::Null) => None,
        Some(v) => match v.as_f64() {
            Some(n) if n.is_finite() => Some(n),
            _ => return Err("`temperature` must be a finite number".into()),
        },
    };

    Ok(Inputs {
        source,
        question,
        model,
        temperature,
    })
}

/// Read a local image, verify its size and MIME, return a `data:` URI.
async fn encode_local_image(path_str: &str, max_bytes: u64) -> Result<String, String> {
    let path = Path::new(path_str);
    if !path.exists() {
        return Err(format!("File not found: {path_str}"));
    }

    let metadata = tokio::fs::metadata(path)
        .await
        .map_err(|e| format!("Failed to read file metadata: {e}"))?;
    let size = metadata.len();
    if size > max_bytes {
        return Err(format!(
            "Image too large: {size} bytes (max {max_bytes} bytes). Adjust [vision].max_image_bytes if intentional."
        ));
    }

    let bytes = tokio::fs::read(path)
        .await
        .map_err(|e| format!("Failed to read image file: {e}"))?;

    let mime = detect_mime(&bytes);
    if !ALLOWED_MIME_TYPES.contains(&mime) {
        return Err(format!(
            "Unsupported image format for {path_str}: detected `{mime}`. Allowed: {}",
            ALLOWED_MIME_TYPES.join(", ")
        ));
    }

    use base64::Engine;
    let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Ok(format!("data:{mime};base64,{encoded}"))
}

/// Detect MIME from magic bytes. Returns a canonical MIME or
/// `application/octet-stream` for unknown formats.
fn detect_mime(bytes: &[u8]) -> &'static str {
    if bytes.len() < 4 {
        return "application/octet-stream";
    }
    if bytes.starts_with(b"\x89PNG") {
        "image/png"
    } else if bytes.starts_with(b"\xFF\xD8\xFF") {
        "image/jpeg"
    } else if bytes.starts_with(b"GIF8") {
        "image/gif"
    } else if bytes.starts_with(b"RIFF") && bytes.len() >= 12 && &bytes[8..12] == b"WEBP" {
        "image/webp"
    } else if bytes.starts_with(b"BM") {
        "image/bmp"
    } else {
        "application/octet-stream"
    }
}

/// Build the multimodal user message with an inline `[IMAGE:...]` marker.
/// Provider layer unpacks the marker into native multimodal parts.
fn compose_user_message(question: &str, data_uri: &str) -> String {
    format!("{question}\n[IMAGE:{data_uri}]")
}

fn err(msg: impl Into<String>) -> ToolResult {
    ToolResult {
        success: false,
        output: String::new(),
        error: Some(msg.into()),
    }
}

/// Production factory: delegate to the shared `create_provider_with_options`
/// so `[vision].provider` can name any built-in provider or URL-prefixed
/// form. Resolves the API key via `resolve_api_key` (literal `api_key`
/// wins over `api_key_env`) and applies `timeout_secs` via
/// `ProviderRuntimeOptions`.
fn default_provider_factory(cfg: &VisionConfig) -> anyhow::Result<Arc<dyn Provider>> {
    let credential = resolve_api_key(cfg).ok();
    let name = resolve_provider_name(cfg)?;
    let options = ProviderRuntimeOptions {
        provider_api_url: if cfg.api_url.trim().is_empty() {
            None
        } else {
            Some(cfg.api_url.clone())
        },
        provider_timeout_secs: Some(cfg.timeout_secs),
        ..ProviderRuntimeOptions::default()
    };
    let boxed = create_provider_with_options(&name, credential.as_deref(), &options)?;
    Ok(Arc::from(boxed))
}

/// Resolve the vision API key. Literal `[vision].api_key` wins when set
/// and non-empty; otherwise falls back to the environment variable named
/// by `[vision].api_key_env`. Returns a human-readable error when neither
/// source provides a non-empty value.
fn resolve_api_key(cfg: &VisionConfig) -> Result<String, String> {
    if let Some(literal) = cfg.api_key.as_deref() {
        let trimmed = literal.trim();
        if !trimmed.is_empty() {
            return Ok(trimmed.to_string());
        }
    }
    match std::env::var(&cfg.api_key_env) {
        Ok(v) if !v.trim().is_empty() => Ok(v),
        _ => Err(format!(
            "Missing vision API key: set [vision].api_key in config, \
             or export the {} environment variable",
            cfg.api_key_env
        )),
    }
}

/// Resolve `[vision].provider` into a concrete name accepted by the
/// provider factory. The bare shorthand `"custom"` is expanded to
/// `custom:${api_url}`; other values (built-in names, URL-prefixed forms)
/// are passed through unchanged.
fn resolve_provider_name(cfg: &VisionConfig) -> anyhow::Result<String> {
    let raw = cfg.provider.trim();
    if raw.is_empty() {
        anyhow::bail!(
            "[vision].provider is empty; set it to a provider name (e.g. \"openai\", \
             \"anthropic\", \"gemini\", \"ollama\") or a URL-prefixed form \
             (e.g. \"custom:https://api.example.com/v1\")"
        );
    }
    if raw.eq_ignore_ascii_case("custom") {
        let url = cfg.api_url.trim();
        if url.is_empty() {
            anyhow::bail!(
                "[vision].provider = \"custom\" requires [vision].api_url to be set \
                 (e.g. \"https://api.openai.com/v1\")"
            );
        }
        return Ok(format!("custom:{url}"));
    }
    Ok(raw.to_string())
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::traits::{ChatRequest, ChatResponse, ProviderCapabilities};
    use crate::security::{AutonomyLevel, SecurityPolicy};
    use std::sync::Mutex;

    fn test_security() -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            workspace_dir: std::env::temp_dir(),
            workspace_only: false,
            forbidden_paths: vec![],
            ..SecurityPolicy::default()
        })
    }

    fn restricted_security() -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            workspace_dir: std::env::temp_dir().join("image_read_ws"),
            workspace_only: true,
            forbidden_paths: vec![],
            ..SecurityPolicy::default()
        })
    }

    /// Minimal provider that records every chat_with_system call and replies
    /// with a canned string. Lives in-module to avoid depending on other
    /// crates' test fixtures.
    struct RecordingProvider {
        system: Mutex<Option<String>>,
        message: Mutex<Option<String>>,
        model: Mutex<Option<String>>,
        temperature: Mutex<Option<f64>>,
        reply: String,
    }

    impl RecordingProvider {
        fn new(reply: impl Into<String>) -> Arc<Self> {
            Arc::new(Self {
                system: Mutex::new(None),
                message: Mutex::new(None),
                model: Mutex::new(None),
                temperature: Mutex::new(None),
                reply: reply.into(),
            })
        }
    }

    #[async_trait]
    impl Provider for RecordingProvider {
        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                vision: true,
                native_tool_calling: true,
                ..ProviderCapabilities::default()
            }
        }

        async fn chat_with_system(
            &self,
            system_prompt: Option<&str>,
            message: &str,
            model: &str,
            temperature: f64,
        ) -> anyhow::Result<String> {
            *self.system.lock().unwrap() = system_prompt.map(ToString::to_string);
            *self.message.lock().unwrap() = Some(message.to_string());
            *self.model.lock().unwrap() = Some(model.to_string());
            *self.temperature.lock().unwrap() = Some(temperature);
            Ok(self.reply.clone())
        }

        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<ChatResponse> {
            Ok(ChatResponse {
                text: Some(self.reply.clone()),
                tool_calls: Vec::new(),
                usage: None,
                reasoning_content: None,
            })
        }
    }

    // ── Static metadata ──────────────────────────────────────────

    #[test]
    fn tool_name_and_description() {
        let tool = ImageReadTool::new(test_security(), VisionConfig::default());
        assert_eq!(tool.name(), "image_read");
        assert!(tool.description().contains("image"));
    }

    #[test]
    fn parameters_schema_describes_path_url_question_model() {
        let tool = ImageReadTool::new(test_security(), VisionConfig::default());
        let schema = tool.parameters_schema();
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["path"].is_object());
        assert!(schema["properties"]["url"].is_object());
        assert!(schema["properties"]["question"].is_object());
        assert!(schema["properties"]["model"].is_object());
    }

    #[test]
    fn spec_round_trip() {
        let tool = ImageReadTool::new(test_security(), VisionConfig::default());
        let spec = tool.spec();
        assert_eq!(spec.name, "image_read");
        assert!(spec.parameters.is_object());
    }

    // ── Input validation ─────────────────────────────────────────

    #[tokio::test]
    async fn requires_path_or_url() {
        let tool = ImageReadTool::new(test_security(), VisionConfig::default());
        let result = tool.run(json!({})).await;
        assert!(!result.success);
        assert!(result.error.unwrap().contains("one image source"));
    }

    #[tokio::test]
    async fn rejects_both_path_and_url() {
        let tool = ImageReadTool::new(test_security(), VisionConfig::default());
        let result = tool
            .run(json!({"path": "/tmp/a.png", "url": "https://x/y.png"}))
            .await;
        assert!(!result.success);
        assert!(result.error.unwrap().contains("exactly one"));
    }

    #[tokio::test]
    async fn rejects_disallowed_path() {
        let tool = ImageReadTool::new(restricted_security(), VisionConfig::default());
        let result = tool.run(json!({"path": "/etc/passwd"})).await;
        assert!(!result.success);
        assert!(result.error.unwrap().contains("Path not allowed"));
    }

    #[tokio::test]
    async fn rejects_url_when_allow_url_is_false() {
        let cfg = VisionConfig {
            allow_url: false,
            ..VisionConfig::default()
        };
        let tool = ImageReadTool::new(test_security(), cfg);
        let result = tool.run(json!({"url": "https://example.com/a.png"})).await;
        assert!(!result.success);
        assert!(result.error.unwrap().contains("URL inputs are disabled"));
    }

    #[tokio::test]
    async fn rejects_unsupported_url_scheme() {
        let cfg = VisionConfig {
            allow_url: true,
            ..VisionConfig::default()
        };
        let tool = ImageReadTool::new(test_security(), cfg);
        let result = tool.run(json!({"url": "ftp://example.com/a.png"})).await;
        assert!(!result.success);
        assert!(result.error.unwrap().contains("Unsupported URL scheme"));
    }

    #[tokio::test]
    async fn missing_api_key_returns_readable_error() {
        let temp = tempfile::tempdir().unwrap();
        let image_path = temp.path().join("sample.png");
        std::fs::write(
            &image_path,
            [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'],
        )
        .unwrap();

        // SAFETY: tests run single-threaded under `cargo test`? Not
        // guaranteed — but we use a unique env var name per test to avoid
        // cross-test interference.
        let env_key = "VISION_API_KEY_MISSING_TEST";
        // SAFETY: setting/removing a uniquely-named env var; no other tests
        // reference this name.
        unsafe {
            std::env::remove_var(env_key);
        }

        let cfg = VisionConfig {
            api_key_env: env_key.into(),
            ..VisionConfig::default()
        };
        let tool = ImageReadTool::new(test_security(), cfg);
        let result = tool
            .run(json!({"path": image_path.to_str().unwrap()}))
            .await;
        assert!(!result.success);
        assert!(result.error.unwrap().contains(env_key));
    }

    #[tokio::test]
    async fn rejects_oversized_image() {
        let temp = tempfile::tempdir().unwrap();
        let image_path = temp.path().join("big.png");

        // Write 10 bytes but cap at 4 → size > cap.
        std::fs::write(
            &image_path,
            [
                0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n', 0x00, 0x00,
            ],
        )
        .unwrap();

        let cfg = VisionConfig {
            max_image_bytes: 4,
            ..VisionConfig::default()
        };
        let tool = ImageReadTool::new(test_security(), cfg);
        let result = tool
            .run(json!({"path": image_path.to_str().unwrap()}))
            .await;
        assert!(!result.success);
        assert!(result.error.unwrap().contains("too large"));
    }

    #[tokio::test]
    async fn rejects_unknown_mime() {
        let temp = tempfile::tempdir().unwrap();
        let image_path = temp.path().join("weird.bin");
        std::fs::write(&image_path, b"this is not an image").unwrap();

        let tool = ImageReadTool::new(test_security(), VisionConfig::default());
        let result = tool
            .run(json!({"path": image_path.to_str().unwrap()}))
            .await;
        assert!(!result.success);
        assert!(result.error.unwrap().contains("Unsupported image format"));
    }

    // ── Happy path ───────────────────────────────────────────────

    #[tokio::test]
    async fn happy_path_delegates_to_vision_provider() {
        let temp = tempfile::tempdir().unwrap();
        let image_path = temp.path().join("ok.png");
        std::fs::write(
            &image_path,
            [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'],
        )
        .unwrap();

        let env_key = "VISION_API_KEY_HAPPY_TEST";
        // SAFETY: unique env var name only used in this test.
        unsafe {
            std::env::set_var(env_key, "test-key");
        }

        let cfg = VisionConfig {
            api_key_env: env_key.into(),
            default_model: "vision-mock".into(),
            ..VisionConfig::default()
        };

        let recorder = RecordingProvider::new("A diagram with two boxes.");
        let recorder_for_factory = recorder.clone();
        let tool = ImageReadTool::with_provider_factory(test_security(), cfg, move |_cfg| {
            Ok(recorder_for_factory.clone() as Arc<dyn Provider>)
        });

        let result = tool
            .run(json!({
                "path": image_path.to_str().unwrap(),
                "question": "描述图中内容",
            }))
            .await;

        unsafe {
            std::env::remove_var(env_key);
        }

        assert!(result.success, "error = {:?}", result.error);
        assert_eq!(result.output, "A diagram with two boxes.");

        let system = recorder.system.lock().unwrap().clone().unwrap();
        assert!(system.contains("图像内容分析助手"));

        let user = recorder.message.lock().unwrap().clone().unwrap();
        assert!(user.starts_with("描述图中内容"));
        assert!(user.contains("[IMAGE:data:image/png;base64,"));

        let model = recorder.model.lock().unwrap().clone().unwrap();
        assert_eq!(model, "vision-mock");
    }

    #[tokio::test]
    async fn honours_temperature_override_and_falls_back_to_config_default() {
        let temp = tempfile::tempdir().unwrap();
        let image_path = temp.path().join("t.png");
        std::fs::write(
            &image_path,
            [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'],
        )
        .unwrap();

        let env_key = "VISION_API_KEY_TEMP_TEST";
        unsafe {
            std::env::set_var(env_key, "k");
        }

        let cfg = VisionConfig {
            api_key_env: env_key.into(),
            default_temperature: 0.9,
            ..VisionConfig::default()
        };

        // Case 1: no override → config default is forwarded.
        let recorder = RecordingProvider::new("ok");
        let recorder_for_factory = recorder.clone();
        let tool =
            ImageReadTool::with_provider_factory(test_security(), cfg.clone(), move |_cfg| {
                Ok(recorder_for_factory.clone() as Arc<dyn Provider>)
            });
        let result = tool
            .run(json!({"path": image_path.to_str().unwrap()}))
            .await;
        assert!(result.success, "error = {:?}", result.error);
        assert_eq!(*recorder.temperature.lock().unwrap(), Some(0.9));

        // Case 2: explicit override wins.
        let recorder2 = RecordingProvider::new("ok");
        let recorder2_for_factory = recorder2.clone();
        let tool2 = ImageReadTool::with_provider_factory(test_security(), cfg, move |_cfg| {
            Ok(recorder2_for_factory.clone() as Arc<dyn Provider>)
        });
        let result = tool2
            .run(json!({
                "path": image_path.to_str().unwrap(),
                "temperature": 0.1,
            }))
            .await;
        assert!(result.success, "error = {:?}", result.error);
        assert_eq!(*recorder2.temperature.lock().unwrap(), Some(0.1));

        unsafe {
            std::env::remove_var(env_key);
        }
    }

    #[tokio::test]
    async fn honours_model_override_and_custom_system_prompt() {
        let temp = tempfile::tempdir().unwrap();
        let image_path = temp.path().join("ok.jpg");
        std::fs::write(&image_path, [0xFF, 0xD8, 0xFF, 0xE0, 0x00]).unwrap();

        let env_key = "VISION_API_KEY_OVERRIDE_TEST";
        unsafe {
            std::env::set_var(env_key, "test-key");
        }

        let cfg = VisionConfig {
            api_key_env: env_key.into(),
            default_model: "default-model".into(),
            system_prompt: Some("custom system".into()),
            ..VisionConfig::default()
        };

        let recorder = RecordingProvider::new("ok");
        let recorder_for_factory = recorder.clone();
        let tool = ImageReadTool::with_provider_factory(test_security(), cfg, move |_cfg| {
            Ok(recorder_for_factory.clone() as Arc<dyn Provider>)
        });

        let result = tool
            .run(json!({
                "path": image_path.to_str().unwrap(),
                "model": "override-model",
            }))
            .await;

        unsafe {
            std::env::remove_var(env_key);
        }

        assert!(result.success);
        assert_eq!(
            recorder.model.lock().unwrap().clone().unwrap(),
            "override-model"
        );
        assert_eq!(
            recorder.system.lock().unwrap().clone().unwrap(),
            "custom system"
        );
    }

    // ── Pure helper tests ────────────────────────────────────────

    #[test]
    fn detect_mime_png() {
        assert_eq!(detect_mime(b"\x89PNG\r\n\x1a\n"), "image/png");
    }

    #[test]
    fn detect_mime_jpeg() {
        assert_eq!(detect_mime(b"\xFF\xD8\xFF\xE0"), "image/jpeg");
    }

    #[test]
    fn detect_mime_webp() {
        let mut bytes = b"RIFF\x00\x00\x00\x00WEBP".to_vec();
        bytes.extend_from_slice(b"VP8 ");
        assert_eq!(detect_mime(&bytes), "image/webp");
    }

    #[test]
    fn detect_mime_unknown() {
        assert_eq!(detect_mime(b"xxxx"), "application/octet-stream");
    }

    #[test]
    fn compose_user_message_uses_image_marker() {
        let msg = compose_user_message("hi", "data:image/png;base64,AAAA");
        assert!(msg.starts_with("hi\n[IMAGE:"));
        assert!(msg.ends_with("]"));
    }

    #[test]
    fn parse_inputs_accepts_only_path() {
        let i = parse_inputs(&json!({"path": "/a/b.png"})).unwrap();
        assert_eq!(i.source, Source::Path("/a/b.png".into()));
        assert_eq!(i.question, DEFAULT_QUESTION);
        assert!(i.model.is_none());
    }

    #[test]
    fn parse_inputs_uses_custom_question() {
        let i = parse_inputs(&json!({"path": "/a", "question": "q"})).unwrap();
        assert_eq!(i.question, "q");
    }

    #[test]
    fn parse_inputs_accepts_temperature_number() {
        let i = parse_inputs(&json!({"path": "/a", "temperature": 0.7})).unwrap();
        assert_eq!(i.temperature, Some(0.7));
    }

    #[test]
    fn parse_inputs_accepts_temperature_integer() {
        let i = parse_inputs(&json!({"path": "/a", "temperature": 1})).unwrap();
        assert_eq!(i.temperature, Some(1.0));
    }

    #[test]
    fn parse_inputs_omits_temperature_when_absent_or_null() {
        let i = parse_inputs(&json!({"path": "/a"})).unwrap();
        assert_eq!(i.temperature, None);
        let i = parse_inputs(&json!({"path": "/a", "temperature": null})).unwrap();
        assert_eq!(i.temperature, None);
    }

    #[test]
    fn parse_inputs_rejects_non_finite_temperature() {
        let err = parse_inputs(&json!({"path": "/a", "temperature": "hot"})).unwrap_err();
        assert!(err.contains("temperature"));
    }

    // ── resolve_provider_name ────────────────────────────────────

    #[test]
    fn resolve_provider_shorthand_custom_expands_with_api_url() {
        let cfg = VisionConfig {
            provider: "custom".into(),
            api_url: "https://api.openai.com/v1".into(),
            ..VisionConfig::default()
        };
        assert_eq!(
            resolve_provider_name(&cfg).unwrap(),
            "custom:https://api.openai.com/v1"
        );
    }

    #[test]
    fn resolve_provider_shorthand_custom_requires_api_url() {
        let cfg = VisionConfig {
            provider: "custom".into(),
            api_url: "".into(),
            ..VisionConfig::default()
        };
        let err = resolve_provider_name(&cfg).unwrap_err().to_string();
        assert!(err.contains("api_url"));
    }

    #[test]
    fn resolve_provider_passthrough_for_builtin_name() {
        let cfg = VisionConfig {
            provider: "anthropic".into(),
            ..VisionConfig::default()
        };
        assert_eq!(resolve_provider_name(&cfg).unwrap(), "anthropic");
    }

    #[test]
    fn resolve_provider_passthrough_for_url_prefixed_form() {
        let cfg = VisionConfig {
            provider: "custom:https://vendor.example.com/v1".into(),
            api_url: "".into(),
            ..VisionConfig::default()
        };
        assert_eq!(
            resolve_provider_name(&cfg).unwrap(),
            "custom:https://vendor.example.com/v1"
        );
    }

    #[test]
    fn resolve_provider_rejects_empty() {
        let cfg = VisionConfig {
            provider: "   ".into(),
            ..VisionConfig::default()
        };
        let err = resolve_provider_name(&cfg).unwrap_err().to_string();
        assert!(err.contains("empty"));
    }

    // ── resolve_api_key ──────────────────────────────────────────

    #[test]
    fn resolve_api_key_prefers_literal_over_env() {
        let env_key = "VISION_API_KEY_RESOLVE_PREFER_TEST";
        unsafe {
            std::env::set_var(env_key, "from-env");
        }
        let cfg = VisionConfig {
            api_key: Some("from-config".into()),
            api_key_env: env_key.into(),
            ..VisionConfig::default()
        };
        assert_eq!(resolve_api_key(&cfg).unwrap(), "from-config");
        unsafe {
            std::env::remove_var(env_key);
        }
    }

    #[test]
    fn resolve_api_key_falls_back_to_env_when_literal_blank() {
        let env_key = "VISION_API_KEY_RESOLVE_FALLBACK_TEST";
        unsafe {
            std::env::set_var(env_key, "env-value");
        }
        let cfg = VisionConfig {
            api_key: Some("   ".into()),
            api_key_env: env_key.into(),
            ..VisionConfig::default()
        };
        assert_eq!(resolve_api_key(&cfg).unwrap(), "env-value");
        unsafe {
            std::env::remove_var(env_key);
        }
    }

    #[test]
    fn resolve_api_key_errors_when_both_unset() {
        let env_key = "VISION_API_KEY_RESOLVE_MISSING_TEST";
        unsafe {
            std::env::remove_var(env_key);
        }
        let cfg = VisionConfig {
            api_key: None,
            api_key_env: env_key.into(),
            ..VisionConfig::default()
        };
        let err = resolve_api_key(&cfg).unwrap_err();
        assert!(err.contains("[vision].api_key"));
        assert!(err.contains(env_key));
    }

    #[tokio::test]
    async fn literal_api_key_bypasses_env() {
        let temp = tempfile::tempdir().unwrap();
        let image_path = temp.path().join("k.png");
        std::fs::write(
            &image_path,
            [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'],
        )
        .unwrap();

        let env_key = "VISION_API_KEY_LITERAL_BYPASS_TEST";
        unsafe {
            std::env::remove_var(env_key);
        }

        let cfg = VisionConfig {
            api_key: Some("inline-key".into()),
            api_key_env: env_key.into(),
            ..VisionConfig::default()
        };

        let recorder = RecordingProvider::new("ok");
        let recorder_for_factory = recorder.clone();
        let tool = ImageReadTool::with_provider_factory(test_security(), cfg, move |_cfg| {
            Ok(recorder_for_factory.clone() as Arc<dyn Provider>)
        });

        let result = tool
            .run(json!({"path": image_path.to_str().unwrap()}))
            .await;
        assert!(result.success, "error = {:?}", result.error);
    }
}
