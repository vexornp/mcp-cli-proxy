# mcp-cli-proxy

A generic-executor MCP server that runs arbitrary shell commands on the host PC, bypassing the agent's sandbox.

The agent runs in a sandboxed environment where some CLI calls and network requests are blocked. The agent **host** (logoscode, Claude Desktop, etc.) launches MCP servers as local subprocesses on your PC — outside the sandbox. `mcp-cli-proxy` exposes a single `exec_command` tool the agent can call to run commands on your PC.

## Install

```sh
cargo install --path .
```

## Run

```sh
mcp-cli-proxy            # run the stdio MCP server (default)
mcp-cli-proxy serve      # same thing, explicit
```

The agent host launches this binary as a subprocess and talks JSON-RPC over stdio, exactly like any other stdio MCP server.

## Register with the agent host

Add `mcp-cli-proxy` to your host's MCP config. Example (logoscode-style):

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
| `timeout_secs` | integer | no | 120 | Per-call timeout. Clamped to `max_timeout_secs` (1800). |
| `stdin` | string | no | — | Bytes piped to stdin. |

Returns JSON: `exit_code` (int|null), `stdout`, `stderr`, `stdout_truncated`, `stderr_truncated`, `timed_out`, `duration_ms`. Each stream is truncated at `output_cap_bytes` (default 100KB).

## Configuration

Resolution order: built-in defaults → config file → environment variables (env wins).

Config file (line-based `key = value`, `#` comments) at `$XDG_CONFIG_HOME/mcp-cli-proxy/config` (default `~/.config/mcp-cli-proxy/config`):

```
output_cap_bytes = 102400
default_timeout_secs = 120
max_timeout_secs = 1800
log_dir = /Users/me/.config/mcp-cli-proxy/logs
```

Equivalent env vars: `MCP_CLI_PROXY_OUTPUT_CAP`, `MCP_CLI_PROXY_DEFAULT_TIMEOUT`, `MCP_CLI_PROXY_MAX_TIMEOUT`, `MCP_CLI_PROXY_LOG_DIR`. Logs go to `<log_dir>/server.log` (set `RUST_LOG` for filtering).

## Security note

This proxy is **unrestricted** by design — it runs any command, any cwd, on your PC. It is intended for a personal dev box you control. The only guard is a per-call timeout (robustness, not a security gate).

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
