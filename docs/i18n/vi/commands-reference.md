# Tham khảo lệnh CclawCore

Dựa trên CLI hiện tại (`cclawcore --help`).

Xác minh lần cuối: **2026-02-20**.

## Lệnh cấp cao nhất

| Lệnh | Mục đích |
|---|---|
| `onboard` | Khởi tạo workspace/config nhanh hoặc tương tác |
| `agent` | Chạy chat tương tác hoặc chế độ gửi tin nhắn đơn |
| `gateway` | Khởi động gateway webhook và HTTP WhatsApp |
| `daemon` | Khởi động runtime có giám sát (gateway + channels + heartbeat/scheduler tùy chọn) |
| `service` | Quản lý vòng đời dịch vụ cấp hệ điều hành |
| `doctor` | Chạy chẩn đoán và kiểm tra trạng thái |
| `status` | Hiển thị cấu hình và tóm tắt hệ thống |
| `cron` | Quản lý tác vụ định kỳ |
| `models` | Làm mới danh mục model của provider |
| `providers` | Liệt kê ID provider, bí danh và provider đang dùng |
| `channel` | Quản lý kênh và kiểm tra sức khỏe kênh |
| `integrations` | Kiểm tra chi tiết tích hợp |
| `skills` | Liệt kê/cài đặt/gỡ bỏ skills |
| `migrate` | Nhập dữ liệu từ runtime khác (hiện hỗ trợ OpenClaw) |
| `config` | Xuất schema cấu hình dạng máy đọc được |
| `completions` | Tạo script tự hoàn thành cho shell ra stdout |
| `hardware` | Phát hiện và kiểm tra phần cứng USB |
| `peripheral` | Cấu hình và nạp firmware thiết bị ngoại vi |

## Nhóm lệnh

### `onboard`

- `cclawcore onboard`
- `cclawcore onboard --channels-only`
- `cclawcore onboard --api-key <KEY> --provider <ID> --memory <sqlite|lucid|markdown|none>`
- `cclawcore onboard --api-key <KEY> --provider <ID> --model <MODEL_ID> --memory <sqlite|lucid|markdown|none>`

### `agent`

- `cclawcore agent`
- `cclawcore agent -m "Hello"`
- `cclawcore agent --provider <ID> --model <MODEL> --temperature <0.0-2.0>`
- `cclawcore agent --peripheral <board:path>`

### `gateway` / `daemon`

- `cclawcore gateway [--host <HOST>] [--port <PORT>]`
- `cclawcore daemon [--host <HOST>] [--port <PORT>]`

### `service`

- `cclawcore service install`
- `cclawcore service start`
- `cclawcore service stop`
- `cclawcore service restart`
- `cclawcore service status`
- `cclawcore service uninstall`

### `cron`

- `cclawcore cron list`
- `cclawcore cron add <expr> [--tz <IANA_TZ>] <command>`
- `cclawcore cron add-at <rfc3339_timestamp> <command>`
- `cclawcore cron add-every <every_ms> <command>`
- `cclawcore cron once <delay> <command>`
- `cclawcore cron remove <id>`
- `cclawcore cron pause <id>`
- `cclawcore cron resume <id>`

### `models`

- `cclawcore models refresh`
- `cclawcore models refresh --provider <ID>`
- `cclawcore models refresh --force`

`models refresh` hiện hỗ trợ làm mới danh mục trực tiếp cho các provider: `openrouter`, `openai`, `anthropic`, `groq`, `mistral`, `deepseek`, `xai`, `together-ai`, `gemini`, `ollama`, `astrai`, `venice`, `fireworks`, `cohere`, `moonshot`, `glm`, `zai`, `qwen` và `nvidia`.

### `channel`

- `cclawcore channel list`
- `cclawcore channel start`
- `cclawcore channel doctor`
- `cclawcore channel bind-telegram <IDENTITY>`
- `cclawcore channel add <type> <json>`
- `cclawcore channel remove <name>`

Lệnh trong chat khi runtime đang chạy (Telegram/Discord):

- `/models`
- `/models <provider>`
- `/model`
- `/model <model-id>`

Channel runtime cũng theo dõi `config.toml` và tự động áp dụng thay đổi cho:
- `default_provider`
- `default_model`
- `default_temperature`
- `api_key` / `api_url` (cho provider mặc định)
- `reliability.*` cài đặt retry của provider

`add/remove` hiện chuyển hướng về thiết lập có hướng dẫn / cấu hình thủ công (chưa hỗ trợ đầy đủ mutator khai báo).

### `integrations`

- `cclawcore integrations info <name>`

### `skills`

- `cclawcore skills list`
- `cclawcore skills install <source>`
- `cclawcore skills remove <name>`

`<source>` chấp nhận git remote (`https://...`, `http://...`, `ssh://...` và `git@host:owner/repo.git`) hoặc đường dẫn cục bộ.

Skill manifest (`SKILL.toml`) hỗ trợ `prompts` và `[[tools]]`; cả hai được đưa vào system prompt của agent khi chạy, giúp model có thể tuân theo hướng dẫn skill mà không cần đọc thủ công.

### `migrate`

- `cclawcore migrate openclaw [--source <path>] [--dry-run]`

### `config`

- `cclawcore config schema`

`config schema` xuất JSON Schema (draft 2020-12) cho toàn bộ hợp đồng `config.toml` ra stdout.

### `completions`

- `cclawcore completions bash`
- `cclawcore completions fish`
- `cclawcore completions zsh`
- `cclawcore completions powershell`
- `cclawcore completions elvish`

`completions` chỉ xuất ra stdout để script có thể được source trực tiếp mà không bị lẫn log/cảnh báo.

### `hardware`

- `cclawcore hardware discover`
- `cclawcore hardware introspect <path>`
- `cclawcore hardware info [--chip <chip_name>]`

### `peripheral`

- `cclawcore peripheral list`
- `cclawcore peripheral add <board> <path>`
- `cclawcore peripheral flash [--port <serial_port>]`
- `cclawcore peripheral setup-uno-q [--host <ip_or_host>]`
- `cclawcore peripheral flash-nucleo`

## Kiểm tra nhanh

Để xác minh nhanh tài liệu với binary hiện tại:

```bash
cclawcore --help
cclawcore <command> --help
```
