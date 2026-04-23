# 视觉工具（`image_read`）配置

CclawCore 提供可选的 **`image_read`** 工具：将图像理解任务委托给可配置的
OpenAI 兼容视觉模型，并把**文字结果**返回给主 agent。这样即便主模型是纯
文本的，也能让 agent 回答"这张截图里是什么"之类的问题。

## 适用场景

- 主 provider 是文本模型，但用户经常发图片。
- 想把图像 I/O 路由到更便宜 / 更合规的视觉模型（如 `gpt-4o-mini`、
  `qwen-vl-plus`），同时主对话保留在另一个模型上。
- 想严格控制图片出境范围：工具默认只允许工作区内的本地文件，URL 是
  opt-in。

## 配置

在 `config.toml` 里新增 `[vision]` 节：

```toml
[vision]
enabled = true
api_url = "https://api.openai.com/v1"
api_key_env = "VISION_API_KEY"   # 环境变量名，不是密钥本身
default_model = "gpt-4o-mini"
timeout_secs = 60
max_image_bytes = 5242880        # 5 MB
allow_url = false                # 只允许工作区本地路径
# system_prompt = "自定义提示词..." # 可选覆盖
```

启动 daemon 前导出密钥：

```bash
export VISION_API_KEY="sk-..."
```

### 字段说明

| 字段 | 类型 | 默认 | 说明 |
|------|------|------|------|
| `enabled` | bool | `false` | 是否注册 `image_read` 工具 |
| `api_url` | string | `https://api.openai.com/v1` | OpenAI 兼容端点 base URL |
| `api_key_env` | string | `VISION_API_KEY` | 存放密钥的环境变量名 |
| `default_model` | string | `gpt-4o-mini` | 默认视觉模型 id（可单次覆写） |
| `timeout_secs` | u64 | `60` | 单次请求超时（秒） |
| `max_image_bytes` | u64 | `5242880` | 超出则拒绝 |
| `allow_url` | bool | `false` | 是否允许 `url` 参数（默认仅本地路径） |
| `system_prompt` | string? | `None` | 覆盖内置的 system prompt |

## 工具参数

```jsonc
{
  "path":     "string — 工作区路径（与 url 互斥）",
  "url":     "string — http(s) URL（需 allow_url=true）",
  "question": "string，可选 — 想让视觉模型回答什么",
  "model":    "string，可选 — 单次覆盖 default_model"
}
```

返回：视觉模型的纯文本答案。失败时 `success = false`，`error` 给出可读
错误信息。

## 示例

描述本地截图：

```jsonc
// tool_call
{ "name": "image_read",
  "arguments": { "path": "screenshots/dashboard.png",
                 "question": "图中数据有哪些异常值？" } }
```

只做 OCR：

```jsonc
{ "name": "image_read",
  "arguments": { "path": "scans/receipt.jpg",
                 "question": "请输出图中所有文字，不要解释。",
                 "model": "gpt-4o" } }
```

远程 URL（需 `allow_url = true`）：

```jsonc
{ "name": "image_read",
  "arguments": { "url": "https://example.com/chart.png",
                 "question": "用表格总结图中每条曲线的趋势。" } }
```

## 安全边界

- `path` 会过 `SecurityPolicy::is_path_allowed` —— 工作区外的路径一律
  拒绝。
- `url` 默认关闭；开启 `allow_url` 后仅接受 `http://` / `https://`。
- 仅接受 `image/png`、`image/jpeg`、`image/webp`、`image/gif`、
  `image/bmp`；未知魔数直接拒绝。
- 文件超过 `max_image_bytes` 会在发起任何网络请求前被拒绝。
- `VISION_API_KEY`（或自定义 env 名）缺失时返回可读错误而不是崩溃。

## 为什么不直接把图注入主对话？

`image_read` 是刻意设计成自包含的：自己读图、自己问视觉模型，只把
**文本**交还给主 agent。这样避免了让下游每个 provider（以及历史压缩 /
摘要链路）都认识"图像 part"，主 agent 也可以保持纯文本。

如果你确实希望主 provider **直接看到图片**，可以用 channel 层支持的
`[IMAGE:...]` 标记协议（见 `src/multimodal.rs`）——那条路径要求主
provider 具备多模态能力。
