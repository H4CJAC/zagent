//! Demo script engine — regex-matched pre-recorded message replay for ws_v2.
//!
//! Each `.json` file in the scripts directory defines a regex pattern and an
//! ordered sequence of WebSocket frames (with per-frame delays) to send when
//! a user message matches.  This allows deterministic, stable demo sessions
//! that bypass the real agent pipeline entirely.

use axum::extract::ws::{Message, WebSocket};
use futures_util::SinkExt;
use regex::Regex;
use serde::Deserialize;
use std::path::Path;

// ── Types ────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct RawScript {
    pattern: String,
    messages: Vec<ScriptMessage>,
}

#[derive(Clone, Deserialize)]
pub struct ScriptMessage {
    #[serde(default)]
    pub delay_ms: u64,
    pub event: serde_json::Value,
}

pub struct DemoScript {
    pub pattern: Regex,
    pub messages: Vec<ScriptMessage>,
}

pub struct DemoScriptEngine {
    scripts: Vec<DemoScript>,
}

// ── Loading ──────────────────────────────────────────────────────────────

const DEFAULT_DIR: &str = ".local/demo-scripts";

impl DemoScriptEngine {
    /// Load all `.json` script files from the given directory.
    /// Files are sorted by name so ordering is deterministic.
    pub fn load(workspace_dir: &Path, custom_dir: Option<&str>) -> anyhow::Result<Self> {
        let dir = match custom_dir {
            Some(d) => {
                let p = Path::new(d);
                if p.is_absolute() {
                    p.to_path_buf()
                } else {
                    workspace_dir.join(d)
                }
            }
            None => workspace_dir.join(DEFAULT_DIR),
        };

        if !dir.is_dir() {
            tracing::debug!("demo scripts dir not found: {}", dir.display());
            return Ok(Self {
                scripts: Vec::new(),
            });
        }

        let mut entries: Vec<_> = std::fs::read_dir(&dir)?
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.path()
                    .extension()
                    .and_then(|ext| ext.to_str())
                    == Some("json")
            })
            .collect();
        entries.sort_by_key(|e| e.file_name());

        let mut scripts = Vec::new();
        for entry in entries {
            let path = entry.path();
            match Self::load_one(&path) {
                Ok(s) => {
                    tracing::info!(
                        pattern = %s.pattern,
                        messages = s.messages.len(),
                        file = %path.display(),
                        "demo script loaded"
                    );
                    scripts.push(s);
                }
                Err(e) => {
                    tracing::warn!(file = %path.display(), "failed to load demo script: {e}");
                }
            }
        }

        tracing::info!(count = scripts.len(), "demo script engine ready");
        Ok(Self { scripts })
    }

    fn load_one(path: &Path) -> anyhow::Result<DemoScript> {
        let content = std::fs::read_to_string(path)?;
        let raw: RawScript = serde_json::from_str(&content)?;
        let pattern = regex::RegexBuilder::new(&raw.pattern)
            .case_insensitive(true)
            .build()?;
        Ok(DemoScript {
            pattern,
            messages: raw.messages,
        })
    }

    /// Return the first script whose pattern matches the given content.
    pub fn try_match(&self, content: &str) -> Option<&DemoScript> {
        self.scripts.iter().find(|s| s.pattern.is_match(content))
    }

    pub fn is_empty(&self) -> bool {
        self.scripts.is_empty()
    }
}

// ── Replay ───────────────────────────────────────────────────────────────

/// Replay a demo script over the WebSocket sender.
///
/// Returns the `full_response` extracted from the last `done` event (if any),
/// for the caller to persist into session history.
pub async fn replay_script(
    sender: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    script: &DemoScript,
) -> String {
    let mut full_response = String::new();

    for msg in &script.messages {
        if msg.delay_ms > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(msg.delay_ms)).await;
        }

        if msg.event.get("type").and_then(|v| v.as_str()) == Some("done") {
            if let Some(resp) = msg.event.get("full_response").and_then(|v| v.as_str()) {
                full_response = resp.to_string();
            }
        }

        let text = msg.event.to_string();
        tracing::debug!(direction = "out", r#type = msg.event["type"].as_str().unwrap_or("?"), "WS v2 demo ← {text}");
        if sender.send(Message::Text(text.into())).await.is_err() {
            tracing::warn!("demo script replay: client disconnected");
            break;
        }
    }

    full_response
}
