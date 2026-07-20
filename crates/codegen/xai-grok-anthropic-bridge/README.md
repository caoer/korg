# xai-grok-anthropic-bridge

Anthropic Messages façade so **Claude Code** can use **Grok** via the official
`xai-grok-sampler` client (not a reimplemented HTTP client).

## Usage

```bash
# Sticky port + dual-pane traffic TUI (default when stdout is a TTY):
export GROK_ANTHROPIC_SERVE_PORT_FILE="$HOME/.grok/anthropic-serve.port"
cargo run -p xai-grok-anthropic-bridge --bin grok-anthropic-serve -- serve \
  --model grok-4.5 --capture-dir /tmp/ab-capture
# Keys: q quit · j/k request list · Tab panes · [/] scroll · g latest · w dump
# → picks free port first time, writes it to the file; next start reuses it
#   (kills prior grok-anthropic-serve on that port first). File is kept on exit.
# Plain logs: add --no-tui

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
| **`GROK_ANTHROPIC_SERVE_PORT_FILE`** | Path to a file storing the decimal listen port (preferred name) |
| `--port-file PATH` | Same as the env (CLI wins if both set) |

Avoid `CCC_PROXY_PORT_GROK_PATH` — too CCC-specific and “path” is ambiguous. The name above matches the binary and states that the value is a **file path**, not the port itself.

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
