# mcp-cli-proxy

A generic-executor MCP server that runs arbitrary shell commands on the host PC, bypassing the agent's sandbox.

The agent runs in a sandboxed environment where some CLI calls and network requests are blocked. The agent **host** (your agent, Claude Desktop, etc.) launches MCP servers as local subprocesses on your PC — outside the sandbox. `mcp-cli-proxy` exposes a single `exec_command` tool the agent can call to run commands on your PC.

## Install

### Homebrew (macOS)

```sh
brew install vexornp/homebrew-tap/mcp-cli-proxy
```

Or, to tap the repo first:

```sh
brew tap vexornp/homebrew-tap
brew install mcp-cli-proxy
```

Prebuilt `aarch64-apple-darwin` and `x86_64-apple-darwin` binaries are published
on each release; the formula auto-updates when a new tag is cut.

### From source

Requires Rust toolchain.

```sh
cargo install --path .
```

## Usage

`mcp-cli-proxy` has two modes that run as separate processes:

### 1. Daemon (unsandboxed, you start it)

    mcp-cli-proxy daemon

Listens on `127.0.0.1:8130` and runs the shell commands the bridge forwards
to it. Start this in a terminal **before** launching the agent that uses the
bridge. It stays in the foreground; Ctrl-C stops it.

### 2. Bridge / MCP server (sandboxed, the agent starts it)

    mcp-cli-proxy serve   # or just: mcp-cli-proxy

Speaks MCP over stdio (what the agent spawns) and forwards each
`exec_command` call to the daemon over localhost TCP. If the daemon is not
running, it exits nonzero with a message pointing at `mcp-cli-proxy daemon`.

### Why two processes?

The agent sandboxes every process it spawns, including this
one. A sandboxed process cannot run host-level commands (network, `curl`,
`pod install`, ...). The daemon runs outside the sandbox (you start it), so
the shell commands it executes escape the sandbox. The bridge, which the
agent spawns, connects to the daemon over `127.0.0.1:8130` (localhost TCP) —
the sandbox permits localhost TCP connect/bind (`allowLocalBinding: true`)
but blocks Unix domain sockets.

## Register with the agent host

Add `mcp-cli-proxy` to your host's MCP config. Example (typical agent config):

```json
{
  "mcpServers": {
    "mcp-cli-proxy": {
      "command": "mcp-cli-proxy"
    }
  }
}
```

## The `exec_command` tool

| Field | Type | Required | Default | Notes |
|---|---|---|---|---|
| `command` | string | yes | — | Shell command, run via `sh -c`. Pipes/globs/redirects work. |
| `cwd` | string | no | proxy's cwd | Working directory. |
| `env` | object<string,string> | no | — | Extra env vars, merged over the inherited environment. |
| `timeout_secs` | integer | no | 3600 | Per-call timeout. Clamped to `max_timeout_secs` (3600). |
| `stdin` | string | no | — | Bytes piped to stdin. |

Returns JSON: `exit_code` (int|null), `stdout`, `stderr`, `stdout_truncated`, `stderr_truncated`, `timed_out`, `duration_ms`. Each stream is truncated at `output_cap_bytes` (default 100KB).

## Configuration

Resolution order: built-in defaults → config file → environment variables (env wins).

Config file (line-based `key = value`, `#` comments) at `$XDG_CONFIG_HOME/mcp-cli-proxy/config` (default `~/.config/mcp-cli-proxy/config`):

```
output_cap_bytes = 102400
default_timeout_secs = 3600
max_timeout_secs = 3600
log_dir = /Users/me/.config/mcp-cli-proxy/logs
```

Equivalent env vars: `MCP_CLI_PROXY_OUTPUT_CAP`, `MCP_CLI_PROXY_DEFAULT_TIMEOUT`, `MCP_CLI_PROXY_MAX_TIMEOUT`, `MCP_CLI_PROXY_LOG_DIR`. Logs go to `<log_dir>/server.log` (set `RUST_LOG` for filtering).

## Security note

This proxy is **unrestricted** by design — it runs any command, any cwd, on your PC. It is intended for a personal dev box you control. The only guard is a per-call timeout (robustness, not a security gate).

## Known limitations

- **No reconnect after daemon restart.** The bridge holds a single persistent
  connection to the daemon for its lifetime. If the daemon restarts, the bridge
  keeps using the dead connection and every `exec_command` fails until the
  agent is restarted — restart the agent to reconnect.
- **Sequential requests.** Calls are serialized over one socket (no concurrent
  in-flight requests). This is a deliberate non-goal per the design spec.
- **Localhost TCP only.** Listens on `127.0.0.1:8130`. Any local process can
  connect (no auth); intended for a personal dev box. Not network-exposed.

## Smoke test

```sh
(printf '%s\n' \
'{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"smoke","version":"0"}}}' \
'{"jsonrpc":"2.0","method":"notifications/initialized"}' \
'{"jsonrpc":"2.0","id":2,"method":"tools/list"}' \
'{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"exec_command","arguments":{"command":"echo smoke"}}}'; sleep 1) | cargo run -q
```

## Tests

```sh
cargo test
```
