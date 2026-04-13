//! Common utilities for the Seewo tool suite.
//!
//! Provides embedded script extraction, shell execution with streaming,
//! session ID generation, shared path helpers, claw_query integration,
//! and common parameter/result helpers reused across multiple sw_ tools.

use crate::agent::loop_::{DraftEvent, TOOL_LIVE_TX};
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

    let tool_name = cmd.to_string();
    let stdout_handle = tokio::spawn(read_stream(
        child_stdout,
        live_tx.clone(),
        tool_name.clone(),
    ));
    let stderr_handle = tokio::spawn(read_stream(child_stderr, live_tx, tool_name));

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
