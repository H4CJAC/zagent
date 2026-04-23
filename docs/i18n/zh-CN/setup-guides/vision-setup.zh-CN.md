# 视觉工具（`image_read`）配置

CclawCore 提供可选的 **`image_read`** 工具：将图像理解任务委托给可配置的
视觉模型，并把**文字结果**返回给主 agent。这样即便主模型是纯文本的，
也能让 agent 回答"这张截图里是什么"之类的问题。工具走标准的 provider
工厂，因此可以用任意内置 provider（`openai`、`anthropic`、`gemini`、
`ollama`、`seewo` …）或自定义 OpenAI 兼容端点。

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
provider = "custom"              # 见下方"Provider 选型"
api_url = "https://api.openai.com/v1"
# 密钥 —— 两种写法二选一：
api_key = "sk-..."               # 字面值；非空时优先于 api_key_env
api_key_env = "VISION_API_KEY"   # 备用：环境变量名
default_model = "gpt-4o-mini"
default_temperature = 0.2        # 可通过工具参数 temperature 单次覆盖
timeout_secs = 60
max_image_bytes = 5242880        # 5 MB
allow_url = false                # 只允许工作区本地路径
# system_prompt = "自定义提示词..." # 可选覆盖
```

### 密钥配置

支持两种方式（同时设置时 `api_key` 胜出）：

- **配置里直写**：`api_key = "sk-..."`，最省事，和主 provider 顶层的
  `api_key` 字段语义一致。使用时**请把配置文件当作敏感文件**，不要
  提交到代码仓库。
- **走环境变量**：`api_key` 留空，设置 `api_key_env = "VISION_API_KEY"`，
  然后导出：
  ```bash
  export VISION_API_KEY="sk-..."
  ```

两者都没给出非空值时，工具会快速失败，错误信息同时提示配置和环境变量。

### 字段说明

| 字段 | 类型 | 默认 | 说明 |
|------|------|------|------|
| `enabled` | bool | `false` | 是否注册 `image_read` 工具 |
| `provider` | string | `custom` | Provider 标识（见下方） |
| `api_url` | string | `https://api.openai.com/v1` | Base URL 覆盖；仅在 `provider` 为裸 name 或 `custom` 简写时生效 |
| `api_key` | string? | `None` | 字面 API key；非空时优先于 `api_key_env` |
| `api_key_env` | string | `VISION_API_KEY` | 环境变量名；`api_key` 未设置时 fallback |
| `default_model` | string | `gpt-4o-mini` | 默认视觉模型 id（可单次覆写） |
| `default_temperature` | f64 | `0.2` | 默认采样温度（可通过工具参数 `temperature` 单次覆盖） |
| `timeout_secs` | u64 | `60` | 单次请求超时（秒） |
| `max_image_bytes` | u64 | `5242880` | 超出则拒绝 |
| `allow_url` | bool | `false` | 是否允许 `url` 参数（默认仅本地路径） |
| `system_prompt` | string? | `None` | 覆盖内置的 system prompt |

### Provider 选型

`provider` 接受任何 provider 工厂（`providers::create_provider_with_options`）认识的值：

| `provider` | 是否使用 `api_url` | 说明 |
|-----------|--------------------|------|
| `custom`（默认） | 是，必填 | 简写 —— 运行时展开为 `custom:${api_url}`。任何 OpenAI 兼容端点首选。 |
| `custom:https://…` | 忽略 | URL 内嵌在 provider 名里，不再读 `api_url`。 |
| `openai` | 是（覆盖默认） | 走 OpenAI 线协议 + Bearer 鉴权。 |
| `ollama` | 是 | 本地 Ollama，设置 `api_url = "http://localhost:11434/v1"`。 |
| `seewo` | 是 | 希沃自定义端点。 |
| `anthropic` / `anthropic:https://…` | 是 / 忽略 | **不推荐** —— 见下方兼容性说明。 |
| `gemini` | 是 | **不推荐**。 |

### `[IMAGE]` 标记兼容性

工具通过 `[IMAGE:data:...;base64,...]` 标记把图像塞进消息。下列 provider
会把标记还原为各自的原生多模态载荷：

- `custom` / `custom:URL` / `openai` —— OpenAI `image_url` content part ✓
- `ollama` —— OpenAI 兼容，原生支持 ✓
- `seewo` —— 原生支持 ✓

**未实现**标记转换的 provider（当前为 Anthropic 和 Gemini）会把标记当成
纯文本透传，基本看不到图：

- `anthropic` —— 期望 Anthropic `image` content block，不认 OpenAI 标记 ✗
- `gemini` —— 期望 `inline_data` part ✗

若确实要用 Anthropic/Gemini 做视觉，建议前面挂一层 OpenAI 兼容代理
（例如 LiteLLM），配置 `provider = "custom:https://your-proxy/v1"`。

## 工具参数

```jsonc
{
  "path":        "string — 工作区路径（与 url 互斥）",
  "url":         "string — http(s) URL（需 allow_url=true）",
  "question":    "string，可选 — 想让视觉模型回答什么",
  "model":       "string，可选 — 单次覆盖 default_model",
  "temperature": "number，可选 — 单次覆盖 default_temperature"
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
- `api_key` 和 `api_key_env` 都没提供非空值时返回可读错误而不是崩溃。
- 如果要提交共享配置，建议只写 `api_key_env` —— `api_key` 是敏感数据，
  一旦写入 toml 请把文件按机密处理。

## 为什么不直接把图注入主对话？

`image_read` 是刻意设计成自包含的：自己读图、自己问视觉模型，只把
**文本**交还给主 agent。这样避免了让下游每个 provider（以及历史压缩 /
摘要链路）都认识"图像 part"，主 agent 也可以保持纯文本。

如果你确实希望主 provider **直接看到图片**，可以用 channel 层支持的
`[IMAGE:...]` 标记协议（见 `src/multimodal.rs`）——那条路径要求主
provider 具备多模态能力。
