//! Output sanitizer — replaces sensitive words in outbound messages.
//!
//! Provides both full-text replacement (`WordReplacer::sanitize`) and a
//! streaming-safe wrapper (`StreamSanitizer`) that buffers partial chunks
//! to avoid missing keywords split across chunk boundaries.

use std::collections::HashMap;

// ── Default built-in rules ──────────────────────────────────────────

const DEFAULT_RULES: &[(&[&str], &str)] = &[(
    &[
        "GPT-4o",
        "GPT-4",
        "GPT-3.5",
        "ChatGPT",
        "OpenAI",
        "Claude",
        "Anthropic",
        "DeepSeek-R1",
        "DeepSeek-V3",
        "DeepSeek",
        "Kimi",
        "Moonshot",
        "通义千问",
        "Qwen",
        "通义",
        "文心一言",
        "文心",
        "ERNIE",
        "豆包",
        "Doubao",
        "ChatGLM",
        "GLM",
        "智谱",
        "Llama",
        "Gemini",
        "Mistral",
        "Groq",
    ],
    "AI 助手",
)];

/// Expand `DEFAULT_RULES` into flat `(pattern, replacement)` pairs.
fn default_flat_rules() -> Vec<(String, String)> {
    let mut out = Vec::new();
    for &(patterns, replacement) in DEFAULT_RULES {
        for &p in patterns {
            out.push((p.to_string(), replacement.to_string()));
        }
    }
    out
}

// ── WordReplacer ────────────────────────────────────────────────────

/// Multi-pattern replacement engine with case-insensitive matching.
#[derive(Debug, Clone)]
pub struct WordReplacer {
    /// (lowercase_pattern, original_pattern_len_in_bytes, replacement)
    rules: Vec<(String, usize, String)>,
    max_pattern_len: usize,
}

impl WordReplacer {
    /// Build from flat `(pattern, replacement)` pairs.
    /// Rules are sorted by pattern length descending so longer matches win.
    pub fn new(mut flat: Vec<(String, String)>) -> Self {
        flat.sort_by(|a, b| b.0.len().cmp(&a.0.len()));
        let max_pattern_len = flat.first().map(|(p, _)| p.len()).unwrap_or(0);
        let rules = flat
            .into_iter()
            .map(|(p, r)| {
                let len = p.len();
                (p.to_lowercase(), len, r)
            })
            .collect();
        Self {
            rules,
            max_pattern_len,
        }
    }

    /// Build from default rules merged with user-supplied config rules.
    /// User rules override defaults when the same pattern appears in both.
    pub fn from_config(user_rules: &[crate::config::SanitizeRule]) -> Self {
        let mut map: HashMap<String, String> = HashMap::new();

        for (pattern, replacement) in default_flat_rules() {
            let lower = pattern.to_lowercase();
            map.entry(lower).or_insert(replacement);
        }

        // User rules override: keyed by lowercase pattern.
        for rule in user_rules {
            for p in &rule.patterns {
                map.insert(p.to_lowercase(), rule.replacement.clone());
            }
        }

        // Deduplicate: group by lowercase key, keep one entry per unique pattern.
        let mut seen = HashMap::<String, String>::new();
        for (pattern, replacement) in &map {
            let lower = pattern.to_lowercase();
            seen.entry(lower).or_insert_with(|| replacement.clone());
        }

        let flat: Vec<(String, String)> = seen.into_iter().collect();
        Self::new(flat)
    }

    /// Case-insensitive replacement of all matching patterns in `text`.
    pub fn sanitize(&self, text: &str) -> String {
        if self.rules.is_empty() || text.is_empty() {
            return text.to_string();
        }
        let lower = text.to_lowercase();
        let bytes = lower.as_bytes();
        let text_bytes = text.as_bytes();
        let mut result = String::with_capacity(text.len());
        let mut i = 0;
        while i < bytes.len() {
            let mut matched = false;
            for (pattern, pat_len, replacement) in &self.rules {
                let pat_bytes = pattern.as_bytes();
                if i + pat_len <= bytes.len() && &bytes[i..i + pat_len] == pat_bytes {
                    result.push_str(replacement);
                    i += pat_len;
                    matched = true;
                    break;
                }
            }
            if !matched {
                // Advance one UTF-8 character.
                let ch_len = utf8_char_len(text_bytes[i]);
                result.push_str(&text[i..i + ch_len]);
                i += ch_len;
            }
        }
        result
    }

    pub fn max_pattern_len(&self) -> usize {
        self.max_pattern_len
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }
}

fn utf8_char_len(first_byte: u8) -> usize {
    match first_byte {
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xFF => 4,
        _ => 1,
    }
}

// ── StreamSanitizer ─────────────────────────────────────────────────

/// Streaming-safe wrapper that buffers tail bytes to avoid missing
/// keywords split across chunk boundaries.
#[derive(Debug, Clone)]
pub struct StreamSanitizer {
    replacer: WordReplacer,
    buffer: String,
    hold_back: usize,
}

impl StreamSanitizer {
    pub fn new(replacer: WordReplacer) -> Self {
        let hold_back = replacer.max_pattern_len();
        Self {
            replacer,
            buffer: String::new(),
            hold_back,
        }
    }

    /// Append `chunk` and return the sanitized prefix that is safe to emit.
    /// The tail (up to `hold_back` bytes) is retained in the internal buffer.
    pub fn push(&mut self, chunk: &str) -> String {
        if self.replacer.is_empty() {
            return chunk.to_string();
        }
        self.buffer.push_str(chunk);
        if self.buffer.len() <= self.hold_back {
            return String::new();
        }
        let split_at = self.buffer.len() - self.hold_back;
        // Align to a UTF-8 char boundary.
        let split_at = floor_char_boundary(&self.buffer, split_at);
        if split_at == 0 {
            return String::new();
        }
        let prefix = self.replacer.sanitize(&self.buffer[..split_at]);
        self.buffer.drain(..split_at);
        prefix
    }

    /// Flush and sanitize all remaining buffered content.
    pub fn flush(&mut self) -> String {
        if self.buffer.is_empty() {
            return String::new();
        }
        let out = self.replacer.sanitize(&self.buffer);
        self.buffer.clear();
        out
    }

    /// Discard buffer without emitting (used on stream reset).
    pub fn reset(&mut self) {
        self.buffer.clear();
    }
}

/// Find the largest byte index <= `idx` that is a char boundary.
fn floor_char_boundary(s: &str, idx: usize) -> usize {
    let mut i = idx;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn test_replacer() -> WordReplacer {
        WordReplacer::new(vec![
            ("DeepSeek-R1".into(), "AI 助手".into()),
            ("DeepSeek".into(), "AI 助手".into()),
            ("GPT-4o".into(), "AI 助手".into()),
            ("GPT-4".into(), "AI 助手".into()),
            ("Kimi".into(), "机器人".into()),
        ])
    }

    #[test]
    fn sanitize_full_basic() {
        let r = test_replacer();
        assert_eq!(r.sanitize("我是DeepSeek模型"), "我是AI 助手模型");
        assert_eq!(r.sanitize("使用GPT-4o生成"), "使用AI 助手生成");
    }

    #[test]
    fn sanitize_case_insensitive() {
        let r = test_replacer();
        assert_eq!(r.sanitize("deepseek很强"), "AI 助手很强");
        assert_eq!(r.sanitize("DEEPSEEK很强"), "AI 助手很强");
    }

    #[test]
    fn sanitize_longer_pattern_wins() {
        let r = test_replacer();
        assert_eq!(r.sanitize("我用DeepSeek-R1"), "我用AI 助手");
        assert_eq!(r.sanitize("GPT-4o比GPT-4好"), "AI 助手比AI 助手好");
    }

    #[test]
    fn sanitize_different_replacements() {
        let r = test_replacer();
        assert_eq!(r.sanitize("Kimi和DeepSeek"), "机器人和AI 助手");
    }

    #[test]
    fn sanitize_no_match() {
        let r = test_replacer();
        assert_eq!(r.sanitize("普通文本"), "普通文本");
    }

    #[test]
    fn sanitize_empty() {
        let r = test_replacer();
        assert_eq!(r.sanitize(""), "");
        let empty = WordReplacer::new(vec![]);
        assert_eq!(empty.sanitize("anything"), "anything");
    }

    #[test]
    fn stream_basic() {
        let r = test_replacer();
        let mut s = StreamSanitizer::new(r);
        let mut collected = String::new();
        collected.push_str(&s.push("我是Deep"));
        collected.push_str(&s.push("Seek模型，很好用"));
        collected.push_str(&s.flush());
        assert_eq!(collected, "我是AI 助手模型，很好用");
    }

    #[test]
    fn stream_flush_on_short_input() {
        let r = test_replacer();
        let mut s = StreamSanitizer::new(r);
        let out = s.push("hi");
        assert!(out.is_empty());
        let flushed = s.flush();
        assert_eq!(flushed, "hi");
    }

    #[test]
    fn stream_reset_discards() {
        let r = test_replacer();
        let mut s = StreamSanitizer::new(r);
        let _ = s.push("Deep");
        s.reset();
        assert_eq!(s.flush(), "");
    }

    #[test]
    fn stream_empty_replacer_passthrough() {
        let r = WordReplacer::new(vec![]);
        let mut s = StreamSanitizer::new(r);
        assert_eq!(s.push("hello"), "hello");
        assert_eq!(s.flush(), "");
    }
}
