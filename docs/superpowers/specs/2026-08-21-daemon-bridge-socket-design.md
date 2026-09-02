> **TRANSPORT SUPERSEDED (2026-08-23):** The Unix-socket transport described here is sandbox-blocked. The active transport is localhost TCP (see `docs/superpowers/plans/2026-08-23-daemon-bridge-tcp.md`). Non-transport sections (framing, Executor trait, error model) below remain authoritative.

# Daemon + Bridge: forwarding commands to an unsandboxed host process

**Date:** 2026-08-21
**Status:** Approved (brainstorm), pending implementation plan

## Problem

The agent runs every local MCP server inside a macOS seatbelt (srt) sandbox.
The sandbox policy (`~/.cache/<agent>/sandbox/srt-settings.json`) allows
`/tmp/mcp-*` in `allowUnixSockets` and `/tmp` in `allowWrite`, but denies
writes to `~/.config/**` and restricts outbound TCP to an allowlist of
internal domains (no `localhost`).

`mcp-cli-proxy`'s purpose is to run `sh -c` commands on the host *outside*
the sandbox (per `AGENTS.md`: "for git operations that need network, curl,
builds, pod install"). But because the agent spawns the process, the seatbelt
profile is inherited across `exec`, so the proxy's own `sh -c` children
(`src/exec.rs:67-83`) are sandboxed too. The proxy cannot escape by itself.

The startup EPERM on `~/.config/**` was fixed separately (commit `81f121b`,
log dir falls back to `$TMPDIR`), but that only fixed *startup* — the deeper
caveat remains: **`exec_command`'s children inherit the seatbelt, so the
proxy cannot serve its intended purpose as an unsandboxed escape hatch.**

## Goal

Forward `exec_command` invocations to an unsandboxed daemon process that the
user starts manually, so `sh -c` actually runs on the host. The agent's
existing `mcp-cli-proxy` invocation (default stdio MCP server) keeps working
unchanged from the gateway's perspective; it becomes a thin bridge that
forwards calls over a sandbox-allowed Unix socket.

## Constraints

- The agent cannot spawn the unsandboxed daemon (seatbelt inherits on `exec`).
  → The daemon must be started by the user (or launchd, out of scope here).
- The sandbox allows `UnixStream::connect` to paths matching `/tmp/mcp-*`
  (`allowUnixSockets`), and `/tmp` is read/write-allowed.
  → The bridge (sandboxed) can connect to `/tmp/mcp-cli-proxy.sock`.
- localhost TCP is not in `allowedDomains`.
  → TCP/HTTP transport is not viable; Unix socket only.
- `allowAllUnixSockets: false`.
  → Socket path must match the `/tmp/mcp-*` allowlist entry.
- No gateway config change.
  → Default socket path is a constant both sides agree on; no env wiring
    required from the agent (it already passes no special env to the server).

## Non-goals

- launchd/KeepAlive auto-start. (`Manual only` decided in brainstorming.)
- Shared-secret token auth. (`0600 socket perms` decided; on a single-user
  Mac a token file adds no real protection over the socket's own perms.)
- Silent in-process fallback when the daemon is down. (Decided against; the
  bridge must fail loudly so the user knows to start the daemon.)
- Concurrent in-flight requests over one socket. (MCP calls from a single
  client are sequential in practice; a mutex serializes them. Documented as a
  known limitation; can be lifted later with request IDs if needed.)

## Architecture

```
the agent (sandboxed)                   user's machine (unsandboxed)
┌──────────────────────────┐            ┌─────────────────────────────────┐
│  mcp-cli-proxy           │  ExecParams │  mcp-cli-proxy daemon           │
│  (bridge, default mode)  │ ──────────► │  binds /tmp/mcp-cli-proxy.sock  │
│  stdio JSON-RPC ◄─► rmcp │ ◄────────── │  runs sh -c via run_command     │
│  exec_command → socket   │  ExecResult │  returns ExecResult             │
└──────────────────────────┘            └─────────────────────────────────┘
        connect to /tmp/mcp-cli-proxy.sock (sandbox-allowed via /tmp/mcp-* glob)
```

**Two modes, one binary:**

- `mcp-cli-proxy` (no args) and `mcp-cli-proxy serve` → **bridge mode**.
  Sandbox-safe. Owns the rmcp/MCP layer over stdio (unchanged from today).
  On `tools/call` for `exec_command`, forwards `ExecParams` over the Unix
  socket and awaits `ExecResult`. If the socket connect fails, exits nonzero
  with a clear stderr message pointing at `mcp-cli-proxy daemon`.

- `mcp-cli-proxy daemon` → **daemon mode**. Unsandboxed (user starts it in a
  terminal). Binds `/tmp/mcp-cli-proxy.sock` with `0600` perms, unlinks any
  stale socket file first, accept loop, stays up until killed. Owns
  `ExecConfig` (timeouts, output cap) since it's where commands run. Reuses
  the existing `run_command` logic and the log-dir fallback from `81f121b`.

## Why the bridge owns MCP (not the daemon)

Lower risk. rmcp keeps serving over stdio, where it provably works today.
The daemon stays tiny with **no rmcp/MCP dependency** — just a framed
exec-RPC server. The alternative (dumb byte relay + daemon running rmcp over
a socket) depends on rmcp serving over arbitrary `AsyncRead + AsyncWrite`,
which is unverified. The exec-RPC approach needs no such assumption and keeps
the daemon's dependency surface minimal.

## Components

### `Executor` trait (new, in `src/exec.rs`)

Abstracts "run a command, get a result" so `server.rs::dispatch` stops
calling `run_command` directly.

```rust
pub trait Executor: Send + Sync {
    fn exec<'a>(&'a self, params: ExecParams)
        -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<ExecResult, ExecError>> + Send + 'a>>;
}
```

Manual `Pin<Box<dyn Future>>` return (no `async_trait` crate dependency).
This keeps the trait object-safe so `Arc<dyn Executor>` works. `ProxyServer`
holds `Arc<dyn Executor>`.

- `LocalExecutor { config: ExecConfig }` — wraps the existing `run_command`.
  Used by tests and any future standalone-serve path.
- `RemoteExecutor { stream: UnixStream, lock: Mutex<()>, config: ExecConfig }`
  — sends `ExecParams` over the socket, reads `ExecResult` back. The mutex
  serializes concurrent calls over the single connection.

`ProxyServer` (in `src/server.rs`) changes from `Arc<ExecConfig>` to
`Arc<dyn Executor>`, and `dispatch` calls `executor.exec(params)` instead of
`run_command(params, *config)`. Everything else in `server.rs` (tool schema,
arg validation, MCP plumbing) is unchanged.

### `src/bridge.rs` (new)

- `SOCKET_PATH: &str = "/tmp/mcp-cli-proxy.sock"` (module-level constant;
  matches the `/tmp/mcp-*` allowlist entry).
- `pub async fn connect() -> Result<RemoteExecutor, BridgeError>` —
  `UnixStream::connect(SOCKET_PATH)`. On `NotFound` / `ConnectionRefused`,
  returns a `BridgeError::DaemonDown` whose `Display` is:
  `cannot connect to daemon at /tmp/mcp-cli-proxy.sock (is 'mcp-cli-proxy daemon' running?)`.
- `RemoteExecutor::exec` implementation: lock mutex, write framed request,
  read framed response, deserialize `ExecResult`.

### `src/daemon.rs` (new)

- `pub async fn run_daemon(opts: DaemonOptions) -> Result<(), Box<dyn std::error::Error>>`
  — `DaemonOptions { socket_path: PathBuf, config: ExecConfig }` (testable).
  Resolves `ServerConfig` (for `ExecConfig` + log dir fallback), unlinks a
  stale socket at `opts.socket_path`, binds a `UnixListener` with `0600`
  perms on the socket file, accept loop. Each accepted connection is handled
  on a tokio task: read framed `ExecParams`, call `run_command`, write framed
  `ExecResult`. Logs via tracing to the same resolved log dir as the bridge.
- The CLI `daemon` subcommand builds `DaemonOptions` with the default
  `SOCKET_PATH` and `ServerConfig::resolve()`'s `ExecConfig`.
- On `SIGINT`/`SIGTERM`: unlink the socket file, then exit. (Use
  `tokio::signal`.)
- Reuses `crate::log::resolve_log` — the `resolve_log`/`open_log` helpers
  move from `server.rs` (currently private) into a new `src/log.rs` as
  `pub(crate)` so both bridge and daemon modes share them. (Small refactor;
  keeps the log fallback DRY.)

### `src/cli.rs` (modified)

Add `Daemon` to the `Command` enum:

```rust
#[derive(Subcommand)]
pub enum Command {
    /// Run the stdio MCP server (default).
    Serve,
    /// Run the unsandboxed exec daemon. Start this in a separate terminal.
    Daemon,
}
```

`run` dispatches `Daemon` → `crate::daemon::run_daemon()`. No-arg and `Serve`
→ bridge mode (which is the current `run_server`, modified to construct a
`RemoteExecutor` instead of a `LocalExecutor`).

### Framing helpers (new, in `src/exec.rs` or a new `src/framing.rs`)

Length-prefixed JSON. 4-byte big-endian length + payload bytes.

```rust
pub async fn write_frame<W: AsyncWrite + Unpin>(w: &mut W, bytes: &[u8]) -> io::Result<()>;
pub async fn read_frame<R: AsyncRead + Unpin>(r: &mut R) -> io::Result<Vec<u8>>;
```

Both sides use `serde_json` to (de)serialize `ExecParams` / `ExecResult`
into/out of the frame payload.

## Protocol

- Transport: persistent `UnixStream`, one connection per bridge process.
- Message framing: 4-byte BE length prefix + JSON payload.
- Request payload: `ExecParams` (already `Deserialize`, `src/exec.rs:8`).
- Response payload: `ExecResult` (already `Serialize`, `src/exec.rs:17`).
- No request IDs (single in-flight, mutex-serialized).
- Error at the exec layer (spawn failure, io error) is returned as an
  `ExecError`-shaped JSON object in the frame; the bridge maps it back to
  `ExecError` and then to an MCP `internal_error` (same as today's
  `dispatch` error path at `server.rs:193`).

## Error handling

| Failure | Bridge behavior |
|---|---|
| Daemon not running (connect refused / socket missing) | Exit nonzero, stderr: `mcp-cli-proxy: cannot connect to daemon at /tmp/mcp-cli-proxy.sock (is 'mcp-cli-proxy daemon' running?)` |
| Daemon crashes mid-call (connection reset / EOF) | Return MCP `internal_error` for that call: `exec failed: daemon connection lost mid-call` |
| Frame parse error | Return MCP `internal_error`: `exec failed: bad response from daemon` |
| Daemon: stale socket file | Unlink before bind; log a warning |
| Daemon: bind fails (e.g. another daemon running) | Exit nonzero, stderr: `daemon: cannot bind /tmp/mcp-cli-proxy.sock: {e}` |
| Daemon: SIGINT/SIGTERM | Unlink socket file, exit 0 |

## Testing

- **Framing unit tests** (`src/framing.rs` or `exec.rs`): round-trip
  `write_frame`/`read_frame`, zero-length payload, length prefix correctness.
- **Integration test** (`tests/daemon_bridge.rs`): start a daemon on a temp
  socket path via `DaemonOptions { socket_path: tmp, .. }`, connect a
  `RemoteExecutor` to that path, send `ExecParams { command: "echo hi", .. }`,
  assert `ExecResult.stdout == "hi\n"` and `exit_code == Some(0)`. Also a
  failure case: `command: "exit 7"` → `exit_code == Some(7)`.
- **Timeout test**: `command: "sleep 10"`, `timeout_secs: 1` →
  `timed_out == true`.
- **Existing config tests** (`src/config.rs`) untouched.
- **Smoke test the sandbox assumption early**: before building out, write a
  throwaway check that a sandboxed `mcp-cli-proxy serve` can
  `UnixStream::connect` to a `/tmp/mcp-*` socket a daemon holds open. If the
  sandbox blocks this, the whole design needs revisiting. (High confidence
  it works — `/tmp/mcp-*` is in `allowUnixSockets` and `/tmp` is
  read/write-allowed — but verify before committing to the full build.)

## Risks

1. **Sandbox blocks `UnixStream::connect` to `/tmp/mcp-*`.** Mitigation: the
   early smoke test above. If blocked, fallback options are limited (the
   gateway would need a per-server "no sandbox" option, which doesn't exist
   today) — the design would be blocked on an agent-side change.
2. **rmcp `with_writer` + a custom `AsyncWrite` socket adapter.** Not needed
   here (the bridge owns rmcp over stdio, not over the socket), so this risk
   is avoided by the architecture choice.
3. **Socket file left behind after daemon crash.** Mitigation: daemon unlinks
   stale socket before bind; bridge connect just fails clearly.
4. **Concurrent MCP calls.** Serialized by a mutex. If this ever becomes a
   bottleneck, lift it with request IDs + a response router. Not needed now.

## Out of scope (future work)

- launchd agent generation / auto-start.
- Shared-secret token auth.
- Connection pooling / multiplexed requests.
- A `mcp-cli-proxy status` helper that pings the daemon.
- Windows support (Unix sockets only here; Windows would need a named pipe).
