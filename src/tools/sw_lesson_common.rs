//! Common utilities for the Seewo tool suite.
//!
//! Provides embedded script extraction, shell execution with streaming,
//! session ID generation, shared path helpers, claw_query integration,
//! and common parameter/result helpers reused across multiple sw_ tools.

use crate::agent::loop_::{DraftEvent, TOOL_CALL_ID, TOOL_LIVE_TX};
use anyhow::{Context, Result};
use rust_embed::Embed;
use serde_json::Value;
use std::path::{Path, PathBuf};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use super::traits::ToolResult;

#[derive(Embed)]
#[folder = "assets/sw-skill-scripts/"]
struct SkillScripts;

const EMBEDDED_VERSION: &str = include_str!("../../assets/sw-skill-scripts/VERSION");

/// Ensure embedded skill scripts are extracted to `{workspace}/.local/sw-builtin-scripts/`.
/// Returns the base directory path.
pub fn ensure_skill_scripts(workspace_dir: &Path) -> Result<PathBuf> {
    let target = workspace_dir.join(".local/sw-builtin-scripts");
    let version_file = target.join(".version");

    let need_extract = match std::fs::read_to_string(&version_file) {
        Ok(v) => v.trim() != EMBEDDED_VERSION.trim(),
        Err(_) => true,
    };

    if need_extract {
        for file_path in SkillScripts::iter() {
            let file_path_str = file_path.as_ref();
            if let Some(content) = SkillScripts::get(file_path_str) {
                let dest = target.join(file_path_str);
                if let Some(parent) = dest.parent() {
                    std::fs::create_dir_all(parent)
                        .with_context(|| format!("create dir for {}", dest.display()))?;
                }
                std::fs::write(&dest, content.data.as_ref())
                    .with_context(|| format!("write {}", dest.display()))?;
            }
        }
        std::fs::write(&version_file, EMBEDDED_VERSION.trim()).context("write version marker")?;
    }

    Ok(target)
}

/// Generate an 8-character random hex session ID.
pub fn gen_session_id() -> String {
    let bytes: [u8; 4] = rand::random();
    hex::encode(bytes)
}

/// Build the artifacts directory for a lesson-prep session.
pub fn artifacts_dir(workspace_dir: &Path, session_id: &str) -> PathBuf {
    workspace_dir.join("artifacts/lesson-prep").join(session_id)
}

/// Build the artifacts directory for analysis sessions (classroom obs, student analysis, etc.).
pub fn analysis_artifacts_dir(workspace_dir: &Path, session_id: &str) -> PathBuf {
    workspace_dir.join("artifacts/analysis").join(session_id)
}

// ── Seewo user-info API ─────────────────────────────────────────────

const USER_INFO_URL: &str = "https://edu.seewo.com/api/v2/user/both/info";
const AUTH_APP: &str = "EasiNote5";

/// Fetch the `data` object from the Seewo user-info API.
///
/// Returns `Ok(Value)` with the `data` field on success, or an error message.
pub async fn fetch_sw_user_data(token: &str) -> Result<Value, String> {
    let resp = reqwest::Client::new()
        .get(USER_INFO_URL)
        .header("accept", "*/*")
        .header(
            "Cookie",
            format!("x-auth-token={token}; x-auth-app={AUTH_APP};"),
        )
        .send()
        .await
        .map_err(|e| format!("请求用户信息失败: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!("用户信息接口返回 {}", resp.status()));
    }

    let body: Value = resp
        .json()
        .await
        .map_err(|e| format!("解析响应 JSON 失败: {e}"))?;

    let error_code = body
        .get("error_code")
        .and_then(|v| v.as_i64())
        .unwrap_or(-1);
    if error_code != 0 {
        let msg = body
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown error");
        return Err(format!("用户信息接口错误: {msg}"));
    }

    Ok(body.get("data").cloned().unwrap_or(serde_json::json!({})))
}

/// Fetch current teacher's display identity from the Seewo user-info API.
///
/// Returns a short string like `"教师张三(uid:abc123)"` suitable for embedding
/// in natural-language queries. Falls back to `"当前教师"` on any failure.
pub async fn fetch_teacher_identity(token: &str) -> String {
    let data = match fetch_sw_user_data(token).await {
        Ok(d) => d,
        Err(_) => return "当前教师".into(),
    };

    let name = data
        .get("realName")
        .or_else(|| data.get("nickName"))
        .or_else(|| data.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let uid = data
        .get("thirdUid")
        .or_else(|| data.get("uid"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    match (name.is_empty(), uid.is_empty()) {
        (false, false) => format!("教师{name}(uid:{uid})"),
        (false, true) => format!("教师{name}"),
        (true, false) => format!("当前教师(uid:{uid})"),
        (true, true) => "当前教师".into(),
    }
}

// ── claw_query integration ──────────────────────────────────────────

/// Call `claw_query.py --no-stream` and return the `answer` text.
///
/// Returns `Ok(answer)` on success, or an error-prefixed string on failure.
pub async fn run_claw_query(
    scripts_dir: &Path,
    token: &str,
    question: &str,
    cwd: &Path,
    timeout_secs: u64,
) -> Result<String> {
    let script = scripts_dir
        .join("claw-data-querying/scripts/claw_query.py")
        .to_string_lossy()
        .to_string();

    let (stdout, stderr, success) = run_script(
        "python3",
        &[
            &script,
            "--token",
            token,
            "--question",
            question,
            "--no-stream",
        ],
        &[],
        cwd,
        timeout_secs,
    )
    .await?;

    if !success {
        let detail = if stderr.is_empty() { &stdout } else { &stderr };
        return Ok(format!("[查询失败] {}", detail.trim()));
    }

    let parsed: Value =
        serde_json::from_str(&stdout).unwrap_or_else(|_| serde_json::json!(stdout.trim()));
    let answer = parsed
        .get("answer")
        .and_then(|v| v.as_str())
        .unwrap_or(stdout.trim());
    Ok(answer.to_string())
}

// ── agent-she SSE data query ────────────────────────────────────────

const DEFAULT_AGENT_SHE_BASE_URL: &str = "https://agent-she.seewo.com";
const DEFAULT_CREATE_WORKFLOW_ID: u64 = 997;
const DEFAULT_RUN_WORKFLOW_ID: u64 = 998;

/// Configuration for the agent-she data query API.
#[derive(Debug, Clone)]
pub struct AgentSheConfig {
    pub base_url: String,
    pub create_workflow_id: u64,
    pub run_workflow_id: u64,
    pub cache_ttl_secs: u64,
    pub cache_delay_ms: u64,
}

impl Default for AgentSheConfig {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_AGENT_SHE_BASE_URL.into(),
            create_workflow_id: DEFAULT_CREATE_WORKFLOW_ID,
            run_workflow_id: DEFAULT_RUN_WORKFLOW_ID,
            cache_ttl_secs: 0,
            cache_delay_ms: 0,
        }
    }
}

impl AgentSheConfig {
    pub fn from_seewo_cloud(cfg: &crate::config::schema::SeewoCloudConfig) -> Self {
        Self {
            base_url: cfg
                .data_query_base_url
                .clone()
                .unwrap_or_else(|| DEFAULT_AGENT_SHE_BASE_URL.into()),
            create_workflow_id: cfg
                .data_query_create_workflow_id
                .unwrap_or(DEFAULT_CREATE_WORKFLOW_ID),
            run_workflow_id: cfg
                .data_query_run_workflow_id
                .unwrap_or(DEFAULT_RUN_WORKFLOW_ID),
            cache_ttl_secs: cfg.cache_ttl_secs,
            cache_delay_ms: cfg.cache_delay_ms,
        }
    }
}

/// User metadata extracted from the Seewo user-info API, used to populate
/// the `meta` payload for agent-she queries.
#[derive(Debug, Clone)]
pub struct AgentSheMeta {
    pub school_uid: String,
    pub teacher_uid: String,
    pub teacher_name: String,
    pub stage_name: String,
    pub subject_name: String,
}

/// Build `AgentSheMeta` from a previously fetched user-data `Value`.
pub fn extract_user_meta(data: &Value) -> AgentSheMeta {
    let field = |keys: &[&str]| -> String {
        for k in keys {
            if let Some(v) = data
                .get(*k)
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            {
                return v.to_string();
            }
        }
        String::new()
    };
    AgentSheMeta {
        school_uid: field(&["unitId"]),
        teacher_uid: field(&["uid"]),
        teacher_name: field(&["realName", "nickName", "name"]),
        stage_name: field(&["stageName"]),
        subject_name: field(&["subjectName"]),
    }
}

/// Fetch `AgentSheMeta` from the user-info API in one call.
pub async fn fetch_user_meta(token: &str) -> Result<AgentSheMeta, String> {
    let data = fetch_sw_user_data(token).await?;
    Ok(extract_user_meta(&data))
}

/// Run a single data query against the agent-she SSE API.
///
/// 1. Creates a session via `POST /api/sessions`.
/// 2. Sends the question via `POST /api/sessions/{id}/workflow/{wf}/run/sse`.
/// 3. Consumes the SSE stream, forwarding progress via `ToolChunk`.
/// 4. Returns the final answer content from the `agent_response` event.
pub async fn run_agent_she_query(
    config: &AgentSheConfig,
    token: &str,
    question: &str,
    meta: &AgentSheMeta,
    request_type: &str,
    timeout_secs: u64,
) -> Result<String> {
    let client = reqwest::Client::new();
    let token_header_key = "x-kish-token-key";
    let token_header_val = "x-user-token";

    // Step 1: create session
    let create_url = format!("{}/api/sessions", config.base_url);
    let create_body = serde_json::json!({
        "name": "新对话",
        "workflow_id": config.create_workflow_id,
        "workflow_session_type": "general",
    });

    let create_resp = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        client
            .post(&create_url)
            .header("content-type", "application/json")
            .header(token_header_key, token_header_val)
            .header("x-user-token", token)
            .json(&create_body)
            .send(),
    )
    .await
    .context("创建会话超时")?
    .context("创建会话请求失败")?;

    if !create_resp.status().is_success() {
        anyhow::bail!("创建会话失败: HTTP {}", create_resp.status());
    }

    let create_json: Value = create_resp.json().await.context("解析创建会话响应失败")?;
    let session_id = create_json["data"]["id"]
        .as_u64()
        .ok_or_else(|| anyhow::anyhow!("创建会话响应缺少 data.id"))?;

    // Step 2: run SSE query
    let run_url = format!(
        "{}/api/sessions/{}/workflow/{}/run/sse",
        config.base_url, session_id, config.run_workflow_id
    );

    let run_body = serde_json::json!({
        "content": [{ "type": "text", "text": question }],
        "meta": {
            "source": "",
            "course_id": "",
            "school_uid": meta.school_uid,
            "teacher_uid": meta.teacher_uid,
            "teacher_name": meta.teacher_name,
            "stage_name": meta.stage_name,
            "subject_name": meta.subject_name,
            "selected_data": [],
            "course_name": "",
            "course_time": "",
            "request_type": request_type,
        },
        "role": "user",
    });

    let sse_resp = tokio::time::timeout(
        std::time::Duration::from_secs(timeout_secs),
        client
            .post(&run_url)
            .header("Content-Type", "application/json")
            .header(token_header_key, token_header_val)
            .header("x-user-token", token)
            .json(&run_body)
            .send(),
    )
    .await
    .context("SSE 查询连接超时")?
    .context("SSE 查询请求失败")?;

    if !sse_resp.status().is_success() {
        anyhow::bail!("SSE 查询失败: HTTP {}", sse_resp.status());
    }

    // Step 3: consume SSE stream
    let live_tx: Option<tokio::sync::mpsc::Sender<DraftEvent>> =
        TOOL_LIVE_TX.try_with(|tx| tx.clone()).ok();
    let call_id = TOOL_CALL_ID.try_with(|id| id.clone()).unwrap_or_default();

    let mut final_content = String::new();
    use futures_util::StreamExt;

    let mut stream = sse_resp.bytes_stream();
    let mut leftover = String::new();

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
    loop {
        let chunk = tokio::time::timeout_at(deadline, stream.next()).await;
        match chunk {
            Ok(Some(Ok(bytes))) => {
                leftover.push_str(&String::from_utf8_lossy(&bytes));
            }
            Ok(Some(Err(e))) => {
                tracing::warn!("SSE stream read error: {e}");
                break;
            }
            Ok(None) => break,
            Err(_) => {
                tracing::warn!("SSE stream read timed out");
                break;
            }
        }

        while let Some(newline_pos) = leftover.find('\n') {
            let line = leftover[..newline_pos].trim().to_string();
            leftover = leftover[newline_pos + 1..].to_string();

            if line.is_empty() || !line.starts_with("data: ") {
                continue;
            }
            let json_str = &line["data: ".len()..];
            let Ok(event) = serde_json::from_str::<Value>(json_str) else {
                continue;
            };

            let event_type = event["type"].as_str().unwrap_or("");
            match event_type {
                "agent_progress_message" => {
                    if let Some(display) = event["data"]["message"]["content"]["display"].as_str() {
                        if let Some(ref tx) = live_tx {
                            let _ = tx
                                .send(DraftEvent::ToolChunk {
                                    call_id: call_id.clone(),
                                    name: "agent_she_query".into(),
                                    content: format!("[进度] {display}\n"),
                                })
                                .await;
                        }
                    }
                }
                "agent_stream_message" => {
                    let busi = event["data"]["message"]["busi_type"].as_str().unwrap_or("");
                    if busi == "agent_answering" {
                        if let Some(chunk_text) = event["data"]["message"]["content"].as_str() {
                            if let Some(ref tx) = live_tx {
                                let _ = tx
                                    .send(DraftEvent::ToolChunk {
                                        call_id: call_id.clone(),
                                        name: "agent_she_query".into(),
                                        content: chunk_text.to_string(),
                                    })
                                    .await;
                            }
                        }
                    }
                }
                "agent_response" => {
                    if let Some(content) = event["data"]["data"]["content"].as_str() {
                        final_content = content.to_string();
                    }
                }
                _ => {}
            }
        }
    }

    if final_content.is_empty() {
        anyhow::bail!("SSE 查询未返回有效结果");
    }
    Ok(final_content)
}

// ── Parameter / result helpers ──────────────────────────────────────

/// Extract a required string parameter.
pub fn require_str(args: &Value, key: &str) -> anyhow::Result<String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from)
        .ok_or_else(|| anyhow::anyhow!("缺少必填参数: {key}"))
}

/// Extract an optional string parameter, defaulting to `""`.
pub fn opt_str(args: &Value, key: &str) -> String {
    args.get(key)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

/// Construct a failed `ToolResult`.
pub fn err_result(msg: String) -> ToolResult {
    ToolResult {
        success: false,
        output: String::new(),
        error: Some(msg),
    }
}

/// Get the global Seewo token, returning an error `ToolResult` if unset.
pub fn require_sw_token() -> Result<String, ToolResult> {
    match crate::gateway::sw_state::get_sw_token() {
        Some(t) if !t.is_empty() => Ok(t),
        _ => Err(err_result(
            "Seewo token 未注入，请先通过 /api/user/sw_token 设置".into(),
        )),
    }
}

/// Truncate a string to at most `max_bytes` while respecting UTF-8 char boundaries.
pub fn truncate_str(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

// ── LLM provider config (shared by sw_* tools) ─────────────────────

/// Shared LLM provider configuration for sw_* tools.
///
/// Wraps all parameters needed to create a `ReliableProvider` with retry,
/// provider fallback, model fallback, and API key rotation.
#[derive(Clone)]
pub struct LlmProviderConfig {
    pub provider_name: String,
    pub model: String,
    pub temperature: f64,
    pub api_key: Option<String>,
    pub api_url: Option<String>,
    pub runtime_options: crate::providers::ProviderRuntimeOptions,
    pub reliability: crate::config::ReliabilityConfig,
    pub model_routes: Vec<crate::config::ModelRouteConfig>,
}

impl LlmProviderConfig {
    pub fn create_provider(&self) -> anyhow::Result<Box<dyn crate::providers::Provider>> {
        crate::providers::create_routed_provider_with_options(
            &self.provider_name,
            self.api_key.as_deref(),
            self.api_url.as_deref(),
            &self.reliability,
            &self.model_routes,
            &self.model,
            &self.runtime_options,
        )
    }
}

// ── Disk cache for sw_* data queries ────────────────────────────────

const PREFIX_PARTICLES: &[&str] = &[
    "啊", "嗯", "呃", "哦", "嘿", "喂", "那个", "那", "就是", "然后",
];
const SUFFIX_PARTICLES: &[&str] = &[
    "吧", "呢", "啊", "呀", "哦", "哈", "嘛", "了", "的",
];

fn normalize_for_cache(s: &str) -> String {
    let no_punct: String = s
        .chars()
        .filter(|c| {
            !c.is_ascii_punctuation()
                && !matches!(
                    c,
                    '，' | '。'
                        | '、'
                        | '？'
                        | '！'
                        | '；'
                        | '：'
                        | '\u{201c}'
                        | '\u{201d}'
                        | '（'
                        | '）'
                        | '【'
                        | '】'
                        | '《'
                        | '》'
                )
        })
        .collect();
    no_punct
        .split_whitespace()
        .map(|seg| {
            let mut t = seg;
            for p in PREFIX_PARTICLES {
                t = t.strip_prefix(p).unwrap_or(t);
            }
            for p in SUFFIX_PARTICLES {
                t = t.strip_suffix(p).unwrap_or(t);
            }
            t
        })
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join("")
}

pub fn cache_key(parts: &[&str]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    for p in parts {
        hasher.update(normalize_for_cache(p).as_bytes());
        hasher.update(b"\x00");
    }
    hex::encode(hasher.finalize())
}

/// Disk-backed cache wrapper. Returns cached result when fresh enough,
/// otherwise calls `fetch_fn` and persists the result on success.
pub async fn cached_query<F, Fut>(
    workspace_dir: &Path,
    category: &str,
    key: &str,
    ttl_secs: u64,
    delay_ms: u64,
    fetch_fn: F,
) -> Result<String>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<String>>,
{
    if ttl_secs == 0 {
        return fetch_fn().await;
    }

    let cache_dir = workspace_dir.join(".cache/sw-tools").join(category);
    let cache_file = cache_dir.join(format!("{key}.json"));

    if let Ok(meta) = std::fs::metadata(&cache_file) {
        if let Ok(modified) = meta.modified() {
            let age = modified.elapsed().unwrap_or(std::time::Duration::MAX);
            if age.as_secs() < ttl_secs {
                if let Ok(content) = std::fs::read_to_string(&cache_file) {
                    tracing::info!(
                        category,
                        key,
                        age_secs = age.as_secs(),
                        delay_ms,
                        "sw cache hit"
                    );
                    if delay_ms > 0 {
                        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                    }
                    return Ok(content);
                }
            }
        }
    }

    let result = fetch_fn().await?;

    if let Err(e) = std::fs::create_dir_all(&cache_dir) {
        tracing::warn!("sw cache: failed to create dir {}: {e}", cache_dir.display());
    } else if let Err(e) = std::fs::write(&cache_file, &result) {
        tracing::warn!("sw cache: failed to write {}: {e}", cache_file.display());
    } else {
        tracing::info!(category, key, "sw cache stored");
    }

    Ok(result)
}

pub async fn run_claw_query_cached(
    scripts_dir: &Path,
    token: &str,
    question: &str,
    cwd: &Path,
    timeout_secs: u64,
    workspace_dir: &Path,
    caller: &str,
    cache_ttl_secs: u64,
    cache_delay_ms: u64,
) -> Result<String> {
    let key = cache_key(&[question]);
    let category = format!("claw_query/{caller}");
    cached_query(
        workspace_dir,
        &category,
        &key,
        cache_ttl_secs,
        cache_delay_ms,
        || run_claw_query(scripts_dir, token, question, cwd, timeout_secs),
    )
    .await
}

pub async fn run_agent_she_query_cached(
    config: &AgentSheConfig,
    token: &str,
    question: &str,
    meta: &AgentSheMeta,
    request_type: &str,
    timeout_secs: u64,
    workspace_dir: &Path,
    caller: &str,
    cache_ttl_secs: u64,
    cache_delay_ms: u64,
) -> Result<String> {
    let key = cache_key(&[
        question,
        &meta.school_uid,
        &meta.teacher_uid,
        request_type,
    ]);
    let category = format!("agent_she/{caller}");
    cached_query(
        workspace_dir,
        &category,
        &key,
        cache_ttl_secs,
        cache_delay_ms,
        || run_agent_she_query(config, token, question, meta, request_type, timeout_secs),
    )
    .await
}

// ── Script execution ────────────────────────────────────────────────

/// Run an external script, streaming stdout/stderr via `ToolChunk` events.
/// Returns `(stdout, stderr, success)`.
pub async fn run_script(
    cmd: &str,
    args: &[&str],
    envs: &[(&str, &str)],
    cwd: &Path,
    timeout_secs: u64,
) -> Result<(String, String, bool)> {
    let mut command = Command::new(cmd);
    command
        .args(args)
        .envs(envs.iter().copied())
        .current_dir(cwd)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let mut child = command
        .spawn()
        .with_context(|| format!("spawn {cmd} — is it installed and in PATH?"))?;

    let child_stdout = child.stdout.take();
    let child_stderr = child.stderr.take();
    let live_tx: Option<tokio::sync::mpsc::Sender<DraftEvent>> =
        TOOL_LIVE_TX.try_with(|tx| tx.clone()).ok();
    let call_id = TOOL_CALL_ID.try_with(|id| id.clone()).unwrap_or_default();

    let tool_name = cmd.to_string();
    let stdout_handle = tokio::spawn(read_stream(
        child_stdout,
        live_tx.clone(),
        tool_name.clone(),
        call_id.clone(),
    ));
    let stderr_handle = tokio::spawn(read_stream(child_stderr, live_tx, tool_name, call_id));

    let status = tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), child.wait())
        .await
        .context("script timed out")?
        .context("wait for child process")?;

    let stdout = stdout_handle.await.unwrap_or_default();
    let stderr = stderr_handle.await.unwrap_or_default();

    Ok((stdout, stderr, status.success()))
}

async fn read_stream(
    stream: Option<impl tokio::io::AsyncRead + Unpin + Send + 'static>,
    live_tx: Option<tokio::sync::mpsc::Sender<DraftEvent>>,
    name: String,
    call_id: String,
) -> String {
    let Some(stream) = stream else {
        return String::new();
    };
    let mut reader = BufReader::new(stream);
    let mut buf = String::new();
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                buf.push_str(&line);
                if let Some(ref tx) = live_tx {
                    let _ = tx
                        .send(DraftEvent::ToolChunk {
                            call_id: call_id.clone(),
                            name: name.clone(),
                            content: line.clone(),
                        })
                        .await;
                }
            }
        }
    }
    buf
}
