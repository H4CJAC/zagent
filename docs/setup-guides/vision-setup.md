# Vision (`image_read`) Setup

CclawCore ships with an optional **`image_read`** tool that delegates image
understanding to a configured OpenAI-compatible vision model and returns a
plain-text answer to the main agent. This lets a text-only agent answer
questions about images (screenshots, diagrams, photos, OCR targets)
without needing multimodal capability itself.

## When to enable this

- Your primary provider is text-only, but users send images.
- You want to route only image I/O through a cheaper / regional vision
  model (e.g. `gpt-4o-mini`, `qwen-vl-plus`) while keeping the main chat
  on a different model.
- You need tight control over which images can leave the machine (the
  tool defaults to workspace-local files only; URL input is opt-in).

## Configuration

Add a `[vision]` section to your `config.toml`:

```toml
[vision]
enabled = true
api_url = "https://api.openai.com/v1"
api_key_env = "VISION_API_KEY"   # env var name, not the key itself
default_model = "gpt-4o-mini"
timeout_secs = 60
max_image_bytes = 5242880        # 5 MB
allow_url = false                # only allow workspace-local paths
# system_prompt = "自定义提示词..." # optional override
```

Then export the key in your shell before starting the daemon:

```bash
export VISION_API_KEY="sk-..."
```

### Fields

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | bool | `false` | Register the `image_read` tool |
| `api_url` | string | `https://api.openai.com/v1` | OpenAI-compatible base URL |
| `api_key_env` | string | `VISION_API_KEY` | Name of env var holding the key |
| `default_model` | string | `gpt-4o-mini` | Vision model id (overridable per-call) |
| `timeout_secs` | u64 | `60` | Per-request timeout |
| `max_image_bytes` | u64 | `5242880` | Reject images larger than this |
| `allow_url` | bool | `false` | Permit `url` argument in addition to `path` |
| `system_prompt` | string? | `None` | Override the built-in system prompt |

## Tool schema

```jsonc
{
  "path":     "string — workspace path (xor with `url`)",
  "url":     "string — http(s) URL (requires allow_url = true)",
  "question": "string, optional — what to ask about the image",
  "model":    "string, optional — override default_model for this call"
}
```

Return value: plain text from the vision model. On failure, `success = false`
with a human-readable `error`.

## Examples

Describe a local screenshot:

```jsonc
// tool_call
{ "name": "image_read",
  "arguments": { "path": "screenshots/dashboard.png",
                 "question": "图中数据有哪些异常值？" } }
```

OCR only:

```jsonc
{ "name": "image_read",
  "arguments": { "path": "scans/receipt.jpg",
                 "question": "请输出图中所有文字，不要解释。",
                 "model": "gpt-4o" } }
```

Remote URL (requires `allow_url = true`):

```jsonc
{ "name": "image_read",
  "arguments": { "url": "https://example.com/chart.png",
                 "question": "用表格总结图中每条曲线的趋势。" } }
```

## Security & limits

- `path` inputs are filtered through `SecurityPolicy::is_path_allowed` —
  images outside the workspace are rejected.
- `url` inputs are **disabled** by default; enabling `allow_url` permits
  only `http://` and `https://` schemes.
- Only `image/png`, `image/jpeg`, `image/webp`, `image/gif`, `image/bmp`
  are accepted. Unknown magic bytes are rejected.
- Files exceeding `max_image_bytes` are rejected before any network call.
- If `VISION_API_KEY` (or the configured env var) is missing, the tool
  returns a readable error instead of crashing.

## Why not inject the image into the main chat?

`image_read` is intentionally self-contained: it reads the image, asks the
vision model on its own, and hands back **text** to the main agent. This
avoids having to teach every downstream provider (and every history
compression / summarization path) about image parts, and it lets the
main agent stay text-only.

If you want the main provider to *see* the image directly, use the
`[IMAGE:...]` marker supported by channels (see `src/multimodal.rs`) —
this path requires a multimodal main provider.
