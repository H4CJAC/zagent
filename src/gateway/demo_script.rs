//! Demo script engine — regex-matched pre-recorded message replay for ws_v2.
//!
//! Each `.json` file in the scripts directory defines a regex pattern and an
//! ordered sequence of WebSocket frames (with per-frame delays) to send when
//! a user message matches.  This allows deterministic, stable demo sessions
//! that bypass the real agent pipeline entirely.
//!
//! Template placeholders in serialized event JSON are expanded at replay time
//! (see [`DemoTemplateExpander`]): `{{NOW_ISO_OFFSET_SECS:N}}`,
//! `{{CHOICE:a|b|c}}` (independent random selection per occurrence; use `\|`
//! to escape a literal pipe inside a candidate), optional `{{TODAY}}` /
//! `{{TODAY_CN}}`.

use axum::extract::ws::{Message, WebSocket};
use chrono::Datelike;
use futures_util::SinkExt;
use regex::Regex;
use serde::Deserialize;
use std::path::Path;
use std::sync::LazyLock;

/// Expands `{{NOW_ISO_OFFSET_SECS:N}}` and optional static placeholders in demo script JSON.
///
/// `N` is seconds added to the expander's `base` (set once per replay at construction).
#[derive(Clone, Copy)]
pub struct DemoTemplateExpander {
    base: chrono::DateTime<chrono::Utc>,
}

static NOW_ISO_OFFSET_SECS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\{\{NOW_ISO_OFFSET_SECS:(-?\d+)\}\}").expect("valid regex"));

static CHOICE: LazyLock<Regex> = LazyLock::new(|| {
    // Match body allows any char except `}`, with `\x` escape (e.g. `\|`) so
    // literal `|` can be included inside a candidate.
    Regex::new(r"\{\{CHOICE:((?:\\.|[^}\\])+)\}\}").expect("valid regex")
});

/// Pick a random candidate string; returns `""` for an empty slice.
fn pick_choice(candidates: &[String]) -> String {
    use rand::RngExt;
    if candidates.is_empty() {
        return String::new();
    }
    if candidates.len() == 1 {
        return candidates[0].clone();
    }
    let idx = rand::rng().random_range(0..candidates.len());
    candidates[idx].clone()
}

/// Split a CHOICE body into candidates, honoring `\|` and `\\` escapes.
fn split_choice_body(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = body.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(next) = chars.next() {
                    cur.push(next);
                }
            }
            '|' => {
                out.push(std::mem::take(&mut cur));
            }
            other => cur.push(other),
        }
    }
    out.push(cur);
    out
}

impl DemoTemplateExpander {
    pub fn new() -> Self {
        Self {
            base: chrono::Utc::now(),
        }
    }

    #[cfg(test)]
    fn with_base(base: chrono::DateTime<chrono::Utc>) -> Self {
        Self { base }
    }

    /// Expand template placeholders in a serialized JSON line (or any string).
    pub fn expand(&self, text: &str) -> String {
        let mut s = NOW_ISO_OFFSET_SECS
            .replace_all(text, |caps: &regex::Captures<'_>| {
                let n: i64 = caps[1].parse().unwrap_or(0);
                let t = self.base + chrono::Duration::seconds(n);
                t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
            })
            .into_owned();
        s = CHOICE
            .replace_all(&s, |caps: &regex::Captures<'_>| {
                let candidates = split_choice_body(&caps[1]);
                pick_choice(&candidates)
            })
            .into_owned();
        s = s.replace("{{TODAY}}", &self.base.format("%Y-%m-%d").to_string());
        s = s.replace(
            "{{TODAY_CN}}",
            &format!(
                "{}年{:02}月{:02}日",
                self.base.year(),
                self.base.month(),
                self.base.day()
            ),
        );
        s
    }
}

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
            .filter(|e| e.path().extension().and_then(|ext| ext.to_str()) == Some("json"))
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
    let expander = DemoTemplateExpander::new();
    let mut full_response = String::new();

    for msg in &script.messages {
        if msg.delay_ms > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(msg.delay_ms)).await;
        }

        let raw = msg.event.to_string();
        let wire = expander.expand(&raw);

        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&wire) {
            if v.get("type").and_then(|t| t.as_str()) == Some("done") {
                if let Some(resp) = v.get("full_response").and_then(|x| x.as_str()) {
                    full_response = resp.to_string();
                }
            }
        }

        tracing::debug!(
            direction = "out",
            r#type = msg.event["type"].as_str().unwrap_or("?"),
            "WS v2 demo ← {wire}"
        );
        if sender.send(Message::Text(wire.into())).await.is_err() {
            tracing::warn!("demo script replay: client disconnected");
            break;
        }
    }

    full_response
}

#[cfg(test)]
mod template_tests {
    use super::DemoTemplateExpander;
    use chrono::TimeZone;
    use chrono::Utc;

    #[test]
    fn expand_offset_iso() {
        let base = Utc.with_ymd_and_hms(2026, 4, 24, 12, 0, 0).unwrap();
        let ex = DemoTemplateExpander::with_base(base);
        let s = ex.expand(r#"{"t":"<createdAt>{{NOW_ISO_OFFSET_SECS:31}}</createdAt>"}"#);
        assert!(s.contains("2026-04-24T12:00:31Z"), "got {s}");
        assert!(!s.contains("NOW_ISO_OFFSET"));
    }

    #[test]
    fn expand_negative_offset() {
        let base = Utc.with_ymd_and_hms(2026, 4, 24, 12, 0, 0).unwrap();
        let ex = DemoTemplateExpander::with_base(base);
        let s = ex.expand("{{NOW_ISO_OFFSET_SECS:-3600}}");
        assert_eq!(s, "2026-04-24T11:00:00Z");
    }

    #[test]
    fn expand_today_static() {
        let base = Utc.with_ymd_and_hms(2026, 4, 24, 12, 0, 0).unwrap();
        let ex = DemoTemplateExpander::with_base(base);
        let s = ex.expand("{{TODAY}} / {{TODAY_CN}}");
        assert!(s.contains("2026-04-24"));
        assert!(s.contains("2026年04月24日"));
    }

    #[test]
    fn split_choice_simple() {
        let v = super::split_choice_body("让我|我来|现在我");
        assert_eq!(
            v,
            vec!["让我".to_string(), "我来".to_string(), "现在我".to_string()]
        );
    }

    #[test]
    fn split_choice_with_escape() {
        let v = super::split_choice_body(r"a\|b|c");
        assert_eq!(v, vec!["a|b".to_string(), "c".to_string()]);
    }

    #[test]
    fn choice_replaces_to_one_candidate() {
        let base = Utc.with_ymd_and_hms(2026, 4, 24, 12, 0, 0).unwrap();
        let ex = DemoTemplateExpander::with_base(base);
        let candidates = ["让我", "我来", "现在我"];
        for _ in 0..20 {
            let s = ex.expand("{{CHOICE:让我|我来|现在我}}调用工具");
            assert!(!s.contains("{{CHOICE"), "residual template: {s}");
            let stripped = s.trim_end_matches("调用工具");
            assert!(
                candidates.contains(&stripped),
                "unexpected candidate: {stripped:?}"
            );
        }
    }

    #[test]
    fn choice_and_offset_coexist() {
        let base = Utc.with_ymd_and_hms(2026, 4, 24, 12, 0, 0).unwrap();
        let ex = DemoTemplateExpander::with_base(base);
        let s = ex.expand("{{CHOICE:A|B}} at {{NOW_ISO_OFFSET_SECS:0}}");
        assert!(s.contains("2026-04-24T12:00:00Z"));
        assert!(s.starts_with('A') || s.starts_with('B'));
    }
}
