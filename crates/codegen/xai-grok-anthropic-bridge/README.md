# xai-grok-anthropic-bridge

Anthropic Messages façade so **Claude Code** can use **Grok** via the official
`xai-grok-sampler` client (not a reimplemented HTTP client).

## Install (prebuilt)

Published from GitHub Actions on every `main` push and on `v*` tags.

```bash
# Installer (detects OS/arch → ~/.local/bin)
curl -fsSL https://github.com/caoer/korg/releases/latest/download/install.sh | bash

# Or pin a version tag:
curl -fsSL https://github.com/caoer/korg/releases/latest/download/install.sh | VERSION=v0.1.0 bash

# Re-run is a no-op when the installed binary already matches the desired
# version (from latest.json or VERSION=). Force a re-download with FORCE=1:
curl -fsSL https://github.com/caoer/korg/releases/latest/download/install.sh | FORCE=1 bash
```

### Stable URLs for other apps

| What | URL |
|------|-----|
| **Manifest (JSON)** | https://github.com/caoer/korg/releases/latest/download/latest.json |
| Checksums | https://github.com/caoer/korg/releases/latest/download/SHA256SUMS |
| Installer | https://github.com/caoer/korg/releases/latest/download/install.sh |
| macOS arm64 | https://github.com/caoer/korg/releases/latest/download/grok-anthropic-serve-aarch64-apple-darwin.tar.gz |
| macOS x86_64 | https://github.com/caoer/korg/releases/latest/download/grok-anthropic-serve-x86_64-apple-darwin.tar.gz |
| Linux x86_64 | https://github.com/caoer/korg/releases/latest/download/grok-anthropic-serve-x86_64-unknown-linux-gnu.tar.gz |
| Linux arm64 | https://github.com/caoer/korg/releases/latest/download/grok-anthropic-serve-aarch64-unknown-linux-gnu.tar.gz |
| Release page | https://github.com/caoer/korg/releases/latest |

`latest.json` shape (abbreviated):

```json
{
  "name": "grok-anthropic-serve",
  "version": "0.1.0+abc1234",
  "tag": "latest",
  "git_sha": "...",
  "assets": {
    "aarch64-apple-darwin": {
      "url": "https://github.com/caoer/korg/releases/download/latest/grok-anthropic-serve-aarch64-apple-darwin.tar.gz",
      "latest_url": "https://github.com/caoer/korg/releases/latest/download/grok-anthropic-serve-aarch64-apple-darwin.tar.gz",
      "sha256": "..."
    }
  }
}
```

Example (shell):

```bash
curl -fsSL https://github.com/caoer/korg/releases/latest/download/latest.json \
  | jq -r '.assets["aarch64-apple-darwin"].latest_url'
```

## Usage

```bash
# Sticky port defaults to ~/.grok/anthropic-serve.port (override via env/flag).
# Dual-pane traffic TUI on by default when stdout is a TTY (fullscreen
# alternate screen like vim/grok; logs muted so they don't pollute the TTY):
cargo run -p xai-grok-anthropic-bridge --bin grok-anthropic-serve -- serve \
  --model grok-4.5 --capture-dir /tmp/ab-capture
# Keys: q quit · j/k request list · Tab panes · [/] scroll · g latest · w dump
# → picks free port first time, writes it; next start reuses it (kills prior
#   grok-anthropic-serve on that port). File is kept on exit.
# Plain logs on stderr: --no-tui
# Custom port file: --port-file PATH  or  GROK_ANTHROPIC_SERVE_PORT_FILE=...

# Long-lived serve on a fixed port:
cargo run -p xai-grok-anthropic-bridge --bin grok-anthropic-serve -- serve \
  --port 18766 --model grok-4.5

# Sidecar (spawn serve, run Claude, tear down serve; sticky port file still kept):
cargo run -p xai-grok-anthropic-bridge --bin grok-anthropic-serve -- claude \
  --model grok-4.5

# Manual Claude pointing at an existing serve:
export ANTHROPIC_BASE_URL=http://127.0.0.1:$(cat "$GROK_ANTHROPIC_SERVE_PORT_FILE")
export ANTHROPIC_AUTH_TOKEN=unused
export ANTHROPIC_MODEL=grok-4.5
export ANTHROPIC_SMALL_FAST_MODEL=grok-4.5
export CLAUDE_CODE_DISABLE_NONSTREAMING_FALLBACK=1
claude
```

### Env / naming

| Name | Meaning |
|------|---------|
| **`GROK_ANTHROPIC_SERVE_PORT_FILE`** | Override path to the sticky port file |
| `--port-file PATH` | Same as the env (CLI wins if both set) |
| **Default** | `$HOME/.grok/anthropic-serve.port` (or `$GROK_HOME/anthropic-serve.port`) |

The name matches the binary and states the value is a **file path**, not the port itself.

# Point Claude at the sticky port:
export ANTHROPIC_BASE_URL="http://127.0.0.1:$(cat ~/.grok/anthropic-serve.port)"

(`serve` flags also work without the subcommand for backward compatibility.)

Auth (subscription path uses the **same** shell stack as interactive `grok`):

1. **`LiveSessionAuth`** — `AuthManager` + OIDC refresher + **proactive refresh**
   background task + sampler `bearer_resolver` (per-request live token).
   On 401-looking sampler errors, calls `recover_after_unauthorized` once and retries.
2. Fallback: static `XAI_API_KEY` / session key from disk if LiveSessionAuth cannot start.

Requires a prior `grok login`. Optional: `GROK_CLIENT_VERSION` / `GROK_VERSION`
for cli-chat-proxy version gate (default floor `0.2.106`).

Endpoints: `GET /healthz`, `POST /v1/messages`, `POST /v1/messages/count_tokens`.

## Design notes

- **SessionEpoch**: tools_hash pin + conv/turn/`x-grok-*` headers
- **Reasoning**: encrypted reasoning round-trip via thinking signatures (`grok-bridge:v1:…`)
- **TrafficBus** + `--capture-dir`: dual-side capture for the future debug TUI
- **No** whole-request timeout; uses sampler idle timeout + shared HTTP pool

See home-wiki plan: `grok anthropic-serve` + sidecar + dual-payload TUI.
