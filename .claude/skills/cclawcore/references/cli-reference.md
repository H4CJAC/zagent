# CclawCore CLI Reference

Complete command reference for the `cclawcore` binary.

## Table of Contents

1. [Agent](#agent)
2. [Onboarding](#onboarding)
3. [Status & Diagnostics](#status--diagnostics)
4. [Memory](#memory)
5. [Cron](#cron)
6. [Providers & Models](#providers--models)
7. [Gateway & Daemon](#gateway--daemon)
8. [Service Management](#service-management)
9. [Channels](#channels)
10. [Security & Emergency Stop](#security--emergency-stop)
11. [Hardware Peripherals](#hardware-peripherals)
12. [Skills](#skills)
13. [Shell Completions](#shell-completions)

---

## Agent

Interactive chat or single-message mode.

```bash
cclawcore agent                                          # Interactive REPL
cclawcore agent -m "Summarize today's logs"              # Single message
cclawcore agent -p anthropic --model claude-sonnet-4-6   # Override provider/model
cclawcore agent -t 0.3                                   # Set temperature
cclawcore agent --peripheral nucleo-f401re:/dev/ttyACM0  # Attach hardware
```

**Key flags:**
- `-m <message>` — single message mode (no REPL)
- `-p <provider>` — override provider (openrouter, anthropic, openai, ollama)
- `--model <model>` — override model
- `-t <float>` — temperature (0.0–2.0)
- `--peripheral <name>:<port>` — attach hardware peripheral

The agent has access to 30+ tools gated by security policy: shell, file_read, file_write, file_edit, glob_search, content_search, memory_store, memory_recall, memory_forget, browser, http_request, web_fetch, web_search, cron, delegate, git, and more. Max tool iterations defaults to 10.

---

## Onboarding

First-time setup or reconfiguration.

```bash
cclawcore onboard                                 # Quick mode (default: openrouter)
cclawcore onboard --provider anthropic            # Quick mode with specific provider
cclawcore onboard                                 # Guided wizard (default)
cclawcore onboard --memory sqlite                 # Set memory backend
cclawcore onboard --force                         # Overwrite existing config
cclawcore onboard --channels-only                 # Repair channels only
```

**Key flags:**
- `--provider <name>` — openrouter (default), anthropic, openai, ollama
- `--model <model>` — default model
- `--memory <backend>` — sqlite, markdown, lucid, none
- `--force` — overwrite existing config.toml
- `--channels-only` — only repair channel configuration
- `--reinit` — start fresh (backs up existing config)

Creates `~/.cclawcore/config.toml` with `0600` permissions.

---

## Status & Diagnostics

```bash
cclawcore status                    # System overview
cclawcore doctor                    # Run all diagnostic checks
cclawcore doctor models             # Probe model connectivity
cclawcore doctor traces             # Query execution traces
```

---

## Memory

```bash
cclawcore memory list                              # List all entries
cclawcore memory list --category core --limit 10   # Filtered list
cclawcore memory get "some-key"                    # Get specific entry
cclawcore memory stats                             # Usage statistics
cclawcore memory clear --key "prefix" --yes        # Delete entries (requires --yes)
```

**Key flags:**
- `--category <name>` — filter by category (core, daily, conversation, custom)
- `--limit <n>` — limit results
- `--key <prefix>` — key prefix for clear operations
- `--yes` — skip confirmation (required for clear)

---

## Cron

```bash
cclawcore cron list                                                      # List all jobs
cclawcore cron add '0 9 * * 1-5' 'Good morning' --tz America/New_York   # Recurring (cron expr)
cclawcore cron add-at '2026-03-11T10:00:00Z' 'Remind me about meeting'  # One-time at specific time
cclawcore cron add-every 3600000 'Check server health'                   # Interval in milliseconds
cclawcore cron once 30m 'Follow up on that task'                         # Delay from now
cclawcore cron pause <id>                                                # Pause job
cclawcore cron resume <id>                                               # Resume job
cclawcore cron remove <id>                                               # Delete job
```

**Subcommands:**
- `add <cron-expr> <command>` — standard cron expression (5-field)
- `add-at <iso-datetime> <command>` — fire once at exact time
- `add-every <ms> <command>` — repeating interval
- `once <duration> <command>` — delay from now (e.g., `30m`, `2h`, `1d`)

---

## Providers & Models

```bash
cclawcore providers                                # List all 40+ supported providers
cclawcore models list                              # Show cached model catalog
cclawcore models refresh --all                     # Refresh catalogs from all providers
cclawcore models set anthropic/claude-sonnet-4-6   # Set default model
cclawcore models status                            # Current model info
```

Model routing in config.toml:
```toml
[[model_routes]]
hint = "reasoning"
provider = "openrouter"
model = "anthropic/claude-sonnet-4-6"
```

---

## Gateway & Daemon

```bash
cclawcore gateway                                 # Start HTTP gateway (foreground)
cclawcore gateway -p 8080 --host 127.0.0.1        # Custom port/host

cclawcore daemon                                  # Gateway + channels + scheduler + heartbeat
cclawcore daemon -p 8080 --host 0.0.0.0           # Custom bind
```

**Gateway defaults:**
- Port: 42617
- Host: 127.0.0.1
- Pairing required: true
- Public bind allowed: false

---

## Service Management

OS service lifecycle (systemd on Linux, launchd on macOS).

```bash
cclawcore service install     # Install as system service
cclawcore service start       # Start the service
cclawcore service status      # Check service status
cclawcore service stop        # Stop the service
cclawcore service restart     # Restart the service
cclawcore service uninstall   # Remove the service
```

**Logs:**
- macOS: `~/.cclawcore/logs/daemon.stdout.log`
- Linux: `journalctl -u cclawcore`

---

## Channels

Channels are configured in `config.toml` under `[channels]` and `[channels_config.*]`.

```bash
cclawcore channels list       # List configured channels
cclawcore channels doctor     # Check channel health
```

Supported channels (21 total): Telegram, Discord, Slack, WhatsApp (Meta), WATI, Linq (iMessage/RCS/SMS), Email (IMAP/SMTP), IRC, Matrix, Nostr, Signal, Nextcloud Talk, and more.

Channel config example (Telegram):
```toml
[channels]
telegram = true

[channels_config.telegram]
bot_token = "..."
allowed_users = [123456789]
```

---

## Security & Emergency Stop

```bash
cclawcore estop --level kill-all                              # Stop everything
cclawcore estop --level network-kill                          # Block all network access
cclawcore estop --level domain-block --domain "*.example.com" # Block specific domains
cclawcore estop --level tool-freeze --tool shell              # Freeze specific tool
cclawcore estop status                                        # Check estop state
cclawcore estop resume --network                              # Resume (may require OTP)
```

**Estop levels:**
- `kill-all` — nuclear option, stops all agent activity
- `network-kill` — blocks all outbound network
- `domain-block` — blocks specific domain patterns
- `tool-freeze` — freezes individual tools

Autonomy config in config.toml:
```toml
[autonomy]
level = "supervised"                           # read_only | supervised | full
workspace_only = true
allowed_commands = ["git", "cargo", "python"]
forbidden_paths = ["/etc", "/root", "~/.ssh"]
max_actions_per_hour = 20
max_cost_per_day_cents = 500
```

---

## Hardware Peripherals

```bash
cclawcore hardware discover                              # Find USB devices
cclawcore hardware introspect /dev/ttyACM0               # Probe device capabilities
cclawcore peripheral list                                # List configured peripherals
cclawcore peripheral add nucleo-f401re /dev/ttyACM0      # Add peripheral
cclawcore peripheral flash-nucleo                        # Flash STM32 firmware
cclawcore peripheral flash --port /dev/cu.usbmodem101    # Flash Arduino firmware
```

**Supported boards:** STM32 Nucleo-F401RE, Arduino Uno R4, Raspberry Pi GPIO, ESP32.

Attach to agent session: `cclawcore agent --peripheral nucleo-f401re:/dev/ttyACM0`

---

## Skills

```bash
cclawcore skills list         # List installed skills
cclawcore skills install <path-or-url>  # Install a skill
cclawcore skills audit        # Audit installed skills
cclawcore skills remove <name>  # Remove a skill
```

---

## Shell Completions

```bash
cclawcore completions zsh     # Generate Zsh completions
cclawcore completions bash    # Generate Bash completions
cclawcore completions fish    # Generate Fish completions
```

---

## Config File

Default location: `~/.cclawcore/config.toml`

Config resolution order (first match wins):
1. `CCLAWCORE_CONFIG_DIR` environment variable
2. `CCLAWCORE_WORKSPACE` environment variable
3. `~/.cclawcore/active_workspace.toml` marker file
4. `~/.cclawcore/config.toml` (default)
