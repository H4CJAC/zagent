# CclawCore Commands Reference

This reference is derived from the current CLI surface (`cclawcore --help`).

Last verified: **March 26, 2026**.

## Top-Level Commands

| Command | Purpose |
|---|---|
| `onboard` | Initialize workspace/config quickly or interactively |
| `agent` | Run interactive chat or single-message mode |
| `gateway` | Start webhook and WhatsApp HTTP gateway |
| `acp` | Start ACP (Agent Control Protocol) server over stdio |
| `daemon` | Start supervised runtime (gateway + channels + optional heartbeat/scheduler) |
| `service` | Manage user-level OS service lifecycle |
| `doctor` | Run diagnostics and freshness checks |
| `status` | Print current configuration and system summary |
| `estop` | Engage/resume emergency stop levels and inspect estop state |
| `cron` | Manage scheduled tasks |
| `models` | Refresh provider model catalogs |
| `providers` | List provider IDs, aliases, and active provider |
| `channel` | Manage channels and channel health checks |
| `integrations` | Inspect integration details |
| `skills` | List/install/remove skills |
| `migrate` | Import from external runtimes (currently OpenClaw) |
| `props` | View, set, or initialize config properties |
| `config` | Export machine-readable config schema |
| `completions` | Generate shell completion scripts to stdout |
| `hardware` | Discover and introspect USB hardware |
| `peripheral` | Configure and flash peripherals |

## Command Groups

### `onboard`

- `cclawcore onboard`
- `cclawcore onboard --channels-only`
- `cclawcore onboard --force`
- `cclawcore onboard --reinit`
- `cclawcore onboard --api-key <KEY> --provider <ID> --memory <sqlite|lucid|markdown|none>`
- `cclawcore onboard --api-key <KEY> --provider <ID> --model <MODEL_ID> --memory <sqlite|lucid|markdown|none>`
- `cclawcore onboard --api-key <KEY> --provider <ID> --model <MODEL_ID> --memory <sqlite|lucid|markdown|none> --force`

`onboard` safety behavior:

- If `config.toml` already exists, onboarding offers two modes:
  - Full onboarding (overwrite `config.toml`)
  - Provider-only update (update provider/model/API key while preserving existing channels, tunnel, memory, hooks, and other settings)
- In non-interactive environments, existing `config.toml` causes a safe refusal unless `--force` is passed.
- Use `cclawcore onboard --channels-only` when you only need to rotate channel tokens/allowlists.
- Use `cclawcore onboard --reinit` to start fresh. This backs up your existing config directory with a timestamp suffix and creates a new configuration from scratch.

### `agent`

- `cclawcore agent`
- `cclawcore agent -m "Hello"`
- `cclawcore agent --provider <ID> --model <MODEL> --temperature <0.0-2.0>`
- `cclawcore agent --peripheral <board:path>`

Tip:

- In interactive chat, you can ask for route changes in natural language (for example “conversation uses kimi, coding uses gpt-5.3-codex”); the assistant can persist this via tool `model_routing_config`.

### `acp`

- `cclawcore acp`
- `cclawcore acp --max-sessions <N>`
- `cclawcore acp --session-timeout <SECONDS>`

Start the ACP (Agent Control Protocol) server for IDE and tool integration.

- Uses JSON-RPC 2.0 over stdin/stdout
- Supports methods: `initialize`, `session/new`, `session/prompt`, `session/stop`
- Streams agent reasoning, tool calls, and content in real-time as notifications
- Default max sessions: 10
- Default session timeout: 3600 seconds (1 hour)

### `gateway` / `daemon`

- `cclawcore gateway [--host <HOST>] [--port <PORT>]`
- `cclawcore daemon [--host <HOST>] [--port <PORT>]`

### `estop`

- `cclawcore estop` (engage `kill-all`)
- `cclawcore estop --level network-kill`
- `cclawcore estop --level domain-block --domain "*.chase.com" [--domain "*.paypal.com"]`
- `cclawcore estop --level tool-freeze --tool shell [--tool browser]`
- `cclawcore estop status`
- `cclawcore estop resume`
- `cclawcore estop resume --network`
- `cclawcore estop resume --domain "*.chase.com"`
- `cclawcore estop resume --tool shell`
- `cclawcore estop resume --otp <123456>`

Notes:

- `estop` commands require `[security.estop].enabled = true`.
- When `[security.estop].require_otp_to_resume = true`, `resume` requires OTP validation.
- OTP prompt appears automatically if `--otp` is omitted.

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

Notes:

- Mutating schedule/cron actions require `cron.enabled = true`.
- Shell command payloads for schedule creation (`create` / `add` / `once`) are validated by security command policy before job persistence.

### `models`

- `cclawcore models refresh`
- `cclawcore models refresh --provider <ID>`
- `cclawcore models refresh --force`

`models refresh` currently supports live catalog refresh for provider IDs: `openrouter`, `openai`, `anthropic`, `groq`, `mistral`, `deepseek`, `xai`, `together-ai`, `gemini`, `ollama`, `llamacpp`, `sglang`, `vllm`, `astrai`, `venice`, `fireworks`, `cohere`, `moonshot`, `glm`, `zai`, `qwen`, and `nvidia`.

### `doctor`

- `cclawcore doctor`
- `cclawcore doctor models [--provider <ID>] [--use-cache]`
- `cclawcore doctor traces [--limit <N>] [--event <TYPE>] [--contains <TEXT>]`
- `cclawcore doctor traces --id <TRACE_ID>`

`doctor traces` reads runtime tool/model diagnostics from `observability.runtime_trace_path`.

### `channel`

- `cclawcore channel list`
- `cclawcore channel start`
- `cclawcore channel doctor`
- `cclawcore channel bind-telegram <IDENTITY>`
- `cclawcore channel add <type> <json>`
- `cclawcore channel remove <name>`

Runtime in-chat commands (Telegram/Discord while channel server is running):

- `/models`
- `/models <provider>`
- `/model`
- `/model <model-id>`
- `/new`

Channel runtime also watches `config.toml` and hot-applies updates to:
- `default_provider`
- `default_model`
- `default_temperature`
- `api_key` / `api_url` (for the default provider)
- `reliability.*` provider retry settings

`add/remove` currently route you back to managed setup/manual config paths (not full declarative mutators yet).

### `integrations`

- `cclawcore integrations info <name>`

### `skills`

- `cclawcore skills list`
- `cclawcore skills audit <source_or_name>`
- `cclawcore skills install <source>`
- `cclawcore skills remove <name>`

`<source>` accepts git remotes (`https://...`, `http://...`, `ssh://...`, and `git@host:owner/repo.git`) or a local filesystem path.

`skills install` always runs a built-in static security audit before the skill is accepted. The audit blocks:
- symlinks inside the skill package
- script-like files (`.sh`, `.bash`, `.zsh`, `.ps1`, `.bat`, `.cmd`)
- high-risk command snippets (for example pipe-to-shell payloads)
- markdown links that escape the skill root, point to remote markdown, or target script files

Use `skills audit` to manually validate a candidate skill directory (or an installed skill by name) before sharing it.

Skill manifests (`SKILL.toml`) support `prompts` and `[[tools]]`; both are injected into the agent system prompt at runtime, so the model can follow skill instructions without manually reading skill files.

### `migrate`

- `cclawcore migrate openclaw [--source <path>] [--dry-run]`

### `config`

- `cclawcore config schema`

`config schema` prints a JSON Schema (draft 2020-12) for the full `config.toml` contract to stdout.

### `completions`

- `cclawcore completions bash`
- `cclawcore completions fish`
- `cclawcore completions zsh`
- `cclawcore completions powershell`
- `cclawcore completions elvish`

`completions` is stdout-only by design so scripts can be sourced directly without log/warning contamination.

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

### `props`

Manage individual config properties without editing `config.toml` directly.
Properties are addressed by dotted path (e.g. `channels.matrix.mention-only`).

- `cclawcore props list` — list all properties with current values
- `cclawcore props list --secrets` — list only secret (encrypted) fields
- `cclawcore props list --filter channels.matrix` — filter by path prefix
- `cclawcore props get <path>` — get a single property value (secrets show set/unset status)
- `cclawcore props set <path> <value>` — set a property value
- `cclawcore props set <path>` — secret fields prompt for masked input; enum fields offer interactive selection
- `cclawcore props set --no-interactive <path> <value>` — scripted mode, no prompts
- `cclawcore props init <section>` — create an unconfigured section with defaults (`enabled=false`)
- `cclawcore props init` — initialize all unconfigured sections

Secret fields (API keys, tokens, passwords) are automatically detected via `#[secret]`
annotations. When setting a secret, input is masked regardless of whether a value is
provided on the command line.

Enum fields (e.g. `stream-mode`, `search-mode`) offer interactive selection via arrow
keys when the value is omitted. Provide the value directly to skip the prompt.

Shell tab-completion for property paths is included in `cclawcore completions <shell>`.

#### Adding new config fields

Config structs derive `Configurable` with `#[prefix]` and `#[nested]` attributes.
Adding a new field to an existing struct makes it immediately available via `props`.
New enum types require a one-line `HasPropKind` impl. See `CONTRIBUTING.md` for details.

## Validation Tip

To verify docs against your current binary quickly:

```bash
cclawcore --help
cclawcore <command> --help
```
