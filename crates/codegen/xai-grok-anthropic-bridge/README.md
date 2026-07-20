# xai-grok-anthropic-bridge

Anthropic Messages façade so **Claude Code** can use **Grok** via the official
`xai-grok-sampler` client (not a reimplemented HTTP client).

## Usage

```bash
# From grok-build workspace — long-lived serve:
cargo run -p xai-grok-anthropic-bridge --bin grok-anthropic-serve -- serve \
  --port 18766 --model grok-4.5 --capture-dir /tmp/ab-capture

# Sidecar (spawn serve, run Claude, tear down on exit):
cargo run -p xai-grok-anthropic-bridge --bin grok-anthropic-serve -- claude \
  --model grok-4.5

# Manual Claude pointing at an existing serve:
export ANTHROPIC_BASE_URL=http://127.0.0.1:18766
export ANTHROPIC_AUTH_TOKEN=unused
export ANTHROPIC_MODEL=grok-4.5
export ANTHROPIC_SMALL_FAST_MODEL=grok-4.5
export CLAUDE_CODE_DISABLE_NONSTREAMING_FALLBACK=1
claude
```

(`serve` flags also work without the subcommand for backward compatibility.)

Auth precedence:

1. **Subscription session** from `~/.grok/auth.json` after `grok login` (preferred)
2. Fallback: `XAI_API_KEY` / `GROK_API_KEY` / `--api-key`

No API key is required when a live (non-expired) session `key` is present.
Optional: `GROK_CLIENT_VERSION` / `GROK_VERSION` for cli-chat-proxy version gate
(default floor `0.2.106`).

Endpoints: `GET /healthz`, `POST /v1/messages`, `POST /v1/messages/count_tokens`.

## Design notes

- **SessionEpoch**: tools_hash pin + conv/turn/`x-grok-*` headers
- **Reasoning**: encrypted reasoning round-trip via thinking signatures (`grok-bridge:v1:…`)
- **TrafficBus** + `--capture-dir`: dual-side capture for the future debug TUI
- **No** whole-request timeout; uses sampler idle timeout + shared HTTP pool

See home-wiki plan: `grok anthropic-serve` + sidecar + dual-payload TUI.
