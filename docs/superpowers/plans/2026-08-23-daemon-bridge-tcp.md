# Daemon+Bridge TCP Transport Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Swap the daemon↔bridge transport from Unix domain sockets (sandbox-blocked at both `bind` and `connect`) to localhost TCP so the sandboxed bridge can actually reach the unsandboxed daemon.

**Architecture:** Unchanged from the prior design (daemon + bridge, framed length-prefixed protocol, `Executor` trait, `RemoteExecutor`, `LocalExecutor`, loud-fail on daemon-down). Only the transport changes: `UnixListener`/`UnixStream` → `TcpListener`/`TcpStream`, socket-file path → `127.0.0.1:PORT`. The framing helpers (`src/framing.rs`), `Executor` trait + `LocalExecutor` (`src/exec.rs`), `ProxyServer` dispatch (`src/server.rs`), config (`src/config.rs`), and `run_command` are all unchanged.

**Supersedes:** `docs/superpowers/plans/2026-08-21-daemon-bridge-socket.md` (its Unix-socket design is sandbox-blocked; see Discovery below). The spec `docs/superpowers/specs/2026-08-21-daemon-bridge-socket-design.md` is also superseded for transport specifics; its non-transport sections (framing, trait, error model) remain authoritative.

**Tech Stack:** Rust 2021 (rust-version 1.97), tokio (`"net"` feature already enabled), rmcp, clap, serde, thiserror, tracing.

## Why the change (Discovery)

The logoscode macOS seatbelt sandbox blocks `UnixStream::connect()` to `/tmp/mcp-*` with `EPERM` (errno 1), despite the `allowUnixSockets: ["/tmp/mcp-*"]` policy. Confirmed two ways: (1) the sandboxed bridge prints `mcp-cli-proxy: io error: Operation not permitted (os error 1)` and exits 1; (2) a raw Python `socket.connect()` to the live socket returns `EPERM`. The same sandbox **permits** localhost TCP: `socket.connect(('127.0.0.1', N))` returns `ECONNREFUSED` (errno 61, "nothing listening") not `EPERM`, and `socket.bind(('127.0.0.1', N))` + `listen()` succeed (policy `allowLocalBinding: true`). TCP is the viable transport.

## Global Constraints

- **Transport:** `127.0.0.1:8130` (localhost only, never `0.0.0.0`). Constant `DAEMON_PORT: u16 = 8130`.
- **No silent fallback:** bridge exits 1 with a message naming `mcp-cli-proxy daemon` when the daemon is unreachable (unchanged from prior design).
- **`Executor` trait stays object-safe** (manual `Pin<Box<dyn Future + Send>>`); do NOT add `async_trait` or `async fn` in trait.
- **Framing protocol unchanged:** 4-byte BE u32 length prefix + payload, `MAX_FRAME_BYTES = 64 MiB` (from `src/framing.rs`).
- **Sandbox-permitted syscalls only** in bridge code (the sandboxed half): `TcpStream::connect` to `127.0.0.1:*` is permitted; no `bind`/`listen` in the bridge.
- **Security model:** localhost TCP is reachable by any local process (unlike a 0600 Unix socket). Accepted per the spec's "personal dev box you control" deployment target; document the trade-off. A shared-secret handshake is explicitly out of scope (YAGNI; note as future option).
- **Tests must run in-sandbox:** TCP `bind` is sandbox-permitted, so the 3 previously-`#[ignore]`'d integration tests become normal in-sandbox tests. No `#[ignore]`.
- **Commit style:** conventional commits (`feat:`, `fix:`, `refactor:`, `docs:`, `chore:`) — match `git log --oneline -5`.
- **No comments** unless explicitly requested (project convention).
- **Pre-existing clippy warning** in `tests/exec.rs:131` (`manual_range_contains`) is out of scope; do not touch.

---

## File Structure

| File | Change | Responsibility |
|---|---|---|
| `src/bridge.rs` | rewrite (transport) | `RemoteExecutor` over `TcpStream`, `connect(SocketAddr)`, `DaemonOptions { addr }`, `BridgeError`, `DAEMON_PORT`/`DAEMON_ADDR` |
| `src/daemon.rs` | rewrite (transport) | `run_daemon` over `TcpListener`, `accept_loop`, `handle_conn` over `TcpStream`; drop stale-socket + `set_permissions`; add bound-addr channel for testability |
| `src/server.rs` | targeted edit | `connect(SOCKET_PATH)` → `connect(DAEMON_ADDR)`; update startup log |
| `src/cli.rs` | targeted edit | pass `None` for the new `run_daemon` bound-addr channel arg |
| `tests/daemon_bridge.rs` | rewrite | 3 integration tests (no longer `#[ignore]`) using ephemeral port + bound-addr channel |
| `src/framing.rs` | unchanged | framing helpers (transport-agnostic) |
| `src/exec.rs` | unchanged | `Executor` trait, `LocalExecutor`, `ExecError` (incl. `DaemonConnectionLost`/`BadResponse`), wire derives |
| `src/log.rs` | unchanged | `init_logger`, `resolve_log` |
| `src/config.rs` | unchanged | `ServerConfig::resolve` |
| `Cargo.toml` | unchanged | `"net"` feature already present (covers TCP) |
| `AGENTS.md` | targeted edit | transport = TCP localhost; security note |
| `README.md` | targeted edit | transport = TCP localhost; security note; "Known limitations" update |
| `docs/superpowers/plans/2026-08-21-daemon-bridge-socket.md` | one-line note | mark superseded |
| `docs/superpowers/specs/2026-08-21-daemon-bridge-socket-design.md` | one-line note | mark superseded for transport |

---

### Task 1: Swap transport Unix socket → TCP (bridge.rs, daemon.rs, server.rs, cli.rs)

This is one cohesive task: `bridge.rs` and `daemon.rs` share `DaemonOptions`, so both must change together to compile. Deletes the now-stale `tests/daemon_bridge.rs` (it will be recreated for TCP in Task 2); removing it loses no active coverage (it was entirely `#[ignore]`'d).

**Files:**
- Rewrite: `src/bridge.rs`
- Rewrite: `src/daemon.rs`
- Modify: `src/server.rs:15,41`
- Modify: `src/cli.rs:24-29`
- Delete + leave absent: `tests/daemon_bridge.rs` (recreated in Task 2)

**Interfaces:**
- Consumes: `crate::framing::{read_frame, write_frame}` (unchanged), `crate::exec::{Executor, ExecParams, ExecResult, ExecError, ExecConfig}` (unchanged), `crate::config::ServerConfig` (unchanged), `crate::log::{init_logger, resolve_log}` (unchanged).
- Produces (used by `server.rs` and `cli.rs`):
  - `pub const DAEMON_PORT: u16 = 8130;`
  - `pub const DAEMON_ADDR: SocketAddr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 1), DAEMON_PORT));`
  - `pub async fn connect(addr: SocketAddr) -> Result<RemoteExecutor, BridgeError>`
  - `pub struct DaemonOptions { pub addr: SocketAddr, pub config: ExecConfig }`
  - `impl DaemonOptions { pub fn defaults() -> Self }`
  - `pub async fn run_daemon(opts: DaemonOptions, bound_addr_tx: Option<tokio::sync::oneshot::Sender<std::net::SocketAddr>>) -> Result<(), Box<dyn std::error::Error + Send + Sync>>`

- [ ] **Step 1: Delete the stale integration test file**

```bash
git rm tests/daemon_bridge.rs
```

It was entirely `#[ignore]`'d (sandbox-blocked `UnixListener::bind`); Task 2 recreates it for TCP without `#[ignore]`.

- [ ] **Step 2: Rewrite `src/bridge.rs` with TCP transport**

Full new file content:

```rust
use std::future::Future;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::pin::Pin;
use std::sync::Arc;

use thiserror::Error;
use tokio::net::TcpStream;
use tokio::sync::Mutex;

use crate::config::ServerConfig;
use crate::exec::{ExecConfig, ExecError, ExecParams, ExecResult, Executor};
use crate::framing::{read_frame, write_frame};

pub const DAEMON_PORT: u16 = 8130;

pub const DAEMON_ADDR: SocketAddr = SocketAddr::V4(SocketAddrV4::new(
    Ipv4Addr::new(127, 0, 0, 1),
    DAEMON_PORT,
));

fn is_connection_lost(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::UnexpectedEof
            | std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::BrokenPipe
    )
}

#[derive(Debug, Error)]
pub enum BridgeError {
    #[error("cannot connect to daemon at {addr} (is 'mcp-cli-proxy daemon' running?)")]
    DaemonDown { addr: SocketAddr },
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub struct DaemonOptions {
    pub addr: SocketAddr,
    pub config: ExecConfig,
}

impl DaemonOptions {
    pub fn defaults() -> Self {
        let config = ServerConfig::resolve()
            .map(|c| c.exec)
            .unwrap_or_else(|_| ExecConfig::defaults());
        Self {
            addr: DAEMON_ADDR,
            config,
        }
    }
}

#[derive(Debug)]
pub struct RemoteExecutor {
    stream: Mutex<TcpStream>,
}

pub async fn connect(addr: SocketAddr) -> Result<RemoteExecutor, BridgeError> {
    match TcpStream::connect(addr).await {
        Ok(stream) => Ok(RemoteExecutor {
            stream: Mutex::new(stream),
        }),
        Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => Err(BridgeError::DaemonDown {
            addr,
        }),
        Err(e) => Err(BridgeError::Io(e)),
    }
}

impl Executor for RemoteExecutor {
    fn exec<'a>(
        &'a self,
        params: ExecParams,
    ) -> Pin<Box<dyn Future<Output = Result<ExecResult, ExecError>> + Send + 'a>> {
        Box::pin(async move {
            let mut s = self.stream.lock().await;
            let req = serde_json::to_vec(&params)
                .map_err(|e| ExecError::Spawn(format!("serialize request: {e}")))?;
            write_frame(&mut *s, &req)
                .await
                .map_err(|e| {
                    if is_connection_lost(&e) {
                        ExecError::DaemonConnectionLost
                    } else {
                        ExecError::Spawn(format!("write request: {e}"))
                    }
                })?;
            let resp = match read_frame(&mut *s).await {
                Ok(bytes) => bytes,
                Err(e) if is_connection_lost(&e) => {
                    return Err(ExecError::DaemonConnectionLost);
                }
                Err(_) => {
                    return Err(ExecError::BadResponse);
                }
            };
            let rpc: Result<ExecResult, String> =
                serde_json::from_slice(&resp).map_err(|_| ExecError::BadResponse)?;
            rpc.map_err(ExecError::Spawn)
        })
    }
}

#[cfg(test)]
impl RemoteExecutor {
    fn from_stream(stream: TcpStream) -> Self {
        Self {
            stream: Mutex::new(stream),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn tcp_pair() -> (TcpStream, TcpStream) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let client = TcpStream::connect(addr).await.unwrap();
        let (server, _) = listener.accept().await.unwrap();
        (client, server)
    }

    #[tokio::test]
    async fn remote_executor_round_trips_one_call() {
        let (client, server) = tcp_pair().await;

        let server_task = tokio::spawn(async move {
            let mut conn = server;
            let req = read_frame(&mut conn).await.unwrap();
            let params: ExecParams = serde_json::from_slice(&req).unwrap();
            assert_eq!(params.command, "echo hi");
            let result = ExecResult {
                exit_code: Some(0),
                stdout: "hi\n".into(),
                stderr: String::new(),
                stdout_truncated: false,
                stderr_truncated: false,
                timed_out: false,
                duration_ms: 1,
            };
            let resp = serde_json::to_vec(&Ok::<_, String>(result)).unwrap();
            write_frame(&mut conn, &resp).await.unwrap();
        });

        let exec = RemoteExecutor::from_stream(client);
        let params = ExecParams {
            command: "echo hi".into(),
            ..Default::default()
        };
        let result = exec.exec(params).await.unwrap();
        assert_eq!(result.stdout, "hi\n");
        assert_eq!(result.exit_code, Some(0));
        server_task.await.unwrap();
    }

    #[tokio::test]
    async fn connect_refused_returns_daemon_down() {
        let err = connect(SocketAddr::from(([127, 0, 0, 1], 1)))
            .await
            .unwrap_err();
        assert!(matches!(err, BridgeError::DaemonDown { .. }));
        assert!(err.to_string().contains("mcp-cli-proxy daemon"));
    }
}
```

Key changes vs. prior `bridge.rs`:
- `SOCKET_PATH: &str` → `DAEMON_PORT: u16` + `DAEMON_ADDR: SocketAddr` (const-constructible; `Ipv4Addr::new`/`SocketAddrV4::new` are const fn since Rust 1.69, project floor is 1.97).
- `UnixStream` → `TcpStream`; `connect(path: &Path)` → `connect(addr: SocketAddr)`.
- `DaemonDown` now carries `addr: SocketAddr`; its `Display` interpolates `{addr}`.
- DaemonDown mapping: only `ConnectionRefused` (TCP's "nothing listening"); `NotFound` no longer applies to TCP.
- `DaemonOptions { socket_path: PathBuf }` → `{ addr: SocketAddr, config: ExecConfig }`.
- Unit tests use `tcp_pair()` (bind `127.0.0.1:0`, connect, accept) — sandbox-permitted. The `connect_refused_returns_daemon_down` test connects to port 1 (privileged, guaranteed no listener) → `ConnectionRefused`.
- `Arc` import added (unused here but kept for parity with original; remove if clippy warns — it will, so drop the `use std::sync::Arc;` line if `cargo clippy` flags it).

- [ ] **Step 3: Rewrite `src/daemon.rs` with TCP transport**

Full new file content:

```rust
use std::net::SocketAddr;
use std::sync::Arc;

use tokio::net::{TcpListener, TcpStream};
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::oneshot;

use crate::bridge::DaemonOptions;
use crate::config::ServerConfig;
use crate::exec::{run_command, ExecConfig, ExecParams, ExecResult};
use crate::framing::{read_frame, write_frame};
use crate::log::{init_logger, resolve_log};

pub async fn run_daemon(
    opts: DaemonOptions,
    bound_addr_tx: Option<oneshot::Sender<SocketAddr>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let log_dir_cfg = ServerConfig::resolve()
        .map(|c| c.log_dir)
        .unwrap_or_else(|_| std::env::temp_dir().join("mcp-cli-proxy").join("logs"));
    let (log_dir, log_file) = resolve_log(&log_dir_cfg);
    init_logger(log_file);

    let listener = TcpListener::bind(opts.addr).await?;
    let bound_addr = listener.local_addr()?;
    if let Some(tx) = bound_addr_tx {
        let _ = tx.send(bound_addr);
    }
    tracing::info!(
        "mcp-cli-proxy daemon listening on {}, log_dir={}",
        bound_addr,
        log_dir.display()
    );

    let config = Arc::new(opts.config);

    let mut term = signal(SignalKind::terminate())?;

    tokio::select! {
        _ = accept_loop(&listener, config) => {}
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("daemon: SIGINT received, shutting down");
        }
        _ = term.recv() => {
            tracing::info!("daemon: SIGTERM received, shutting down");
        }
    }

    tracing::info!("daemon stopped");
    Ok(())
}

async fn accept_loop(listener: &TcpListener, config: Arc<ExecConfig>) {
    loop {
        match listener.accept().await {
            Ok((conn, _)) => {
                let config = config.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_conn(conn, &config).await {
                        tracing::warn!("daemon: connection error: {e}");
                    }
                });
            }
            Err(e) => {
                tracing::warn!("daemon: accept error: {e}");
            }
        }
    }
}

async fn handle_conn(
    mut conn: TcpStream,
    config: &ExecConfig,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    loop {
        let req = match read_frame(&mut conn).await {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(e) => return Err(e.into()),
        };
        let params: ExecParams = serde_json::from_slice(&req)?;
        let result = run_command(params, *config).await;
        let rpc: Result<ExecResult, String> = result.map_err(|e| e.to_string());
        let resp = serde_json::to_vec(&rpc)?;
        write_frame(&mut conn, &resp).await?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn tcp_pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let client = TcpStream::connect(addr).await.unwrap();
        let (server, _) = listener.accept().await.unwrap();
        (client, server)
    }

    #[tokio::test]
    async fn handle_conn_round_trips_one_command() {
        let config = ExecConfig::defaults();
        let (mut client, server) = tcp_pair().await;

        let task = tokio::spawn(async move {
            handle_conn(server, &config).await.unwrap();
        });

        let params = ExecParams {
            command: "echo hi".into(),
            ..Default::default()
        };
        let req = serde_json::to_vec(&params).unwrap();
        write_frame(&mut client, &req).await.unwrap();

        let resp = read_frame(&mut client).await.unwrap();
        let rpc: Result<ExecResult, String> = serde_json::from_slice(&resp).unwrap();
        let result = rpc.unwrap();
        assert_eq!(result.stdout, "hi\n");
        assert_eq!(result.exit_code, Some(0));

        drop(client);
        let _ = task.await;
    }

    #[tokio::test]
    async fn handle_conn_reports_nonzero_exit() {
        let config = ExecConfig::defaults();
        let (mut client, server) = tcp_pair().await;

        let task = tokio::spawn(async move {
            handle_conn(server, &config).await.unwrap();
        });

        let params = ExecParams {
            command: "exit 7".into(),
            ..Default::default()
        };
        let req = serde_json::to_vec(&params).unwrap();
        write_frame(&mut client, &req).await.unwrap();

        let resp = read_frame(&mut client).await.unwrap();
        let rpc: Result<ExecResult, String> = serde_json::from_slice(&resp).unwrap();
        let result = rpc.unwrap();
        assert_eq!(result.exit_code, Some(7));

        drop(client);
        let _ = task.await;
    }
}
```

Key changes vs. prior `daemon.rs`:
- `UnixListener`/`UnixStream` → `TcpListener`/`TcpStream`.
- Removed: stale-socket `remove_file`, `set_permissions`, the `std::os::unix::fs::PermissionsExt` import, the TOCTOU comment — none apply to TCP.
- `run_daemon` signature gains `bound_addr_tx: Option<oneshot::Sender<SocketAddr>>` for testability: right after `bind` + `local_addr`, it sends the bound address (so tests using port `0` can learn the actual port). `cli.rs` passes `None`.
- SIGINT + SIGTERM handling unchanged (kept from the review fix in commit `29dd372`).
- Removed the post-shutdown `remove_file(socket_path)` (no socket file with TCP).
- Unit tests use `tcp_pair()` instead of `UnixStream::pair()` (TCP bind is sandbox-permitted).

- [ ] **Step 4: Update `src/server.rs` to connect to `DAEMON_ADDR`**

In `src/server.rs`, change the import and the `connect` call.

Edit the import line (currently `use crate::bridge::{connect, SOCKET_PATH};`) to:

```rust
use crate::bridge::{connect, DAEMON_ADDR};
```

Edit the `connect` call (currently `connect(std::path::Path::new(SOCKET_PATH)).await`) to:

```rust
connect(DAEMON_ADDR).await
```

Leave the rest of `run_server` (the `eprintln!("mcp-cli-proxy: {e}")` + `exit(1)` on error, the startup log line, the `ProxyServer` construction) unchanged. The `std::path::Path` import can be removed if it becomes unused (check with `cargo build`); if clippy warns, drop it.

- [ ] **Step 5: Update `src/cli.rs` to pass `None` for the bound-addr channel**

In `src/cli.rs`, the `Daemon` arm currently calls `crate::daemon::run_daemon(opts).await`. Update to pass `None`:

```rust
Some(Command::Daemon) => {
    let opts = crate::bridge::DaemonOptions::defaults();
    crate::daemon::run_daemon(opts, None)
        .await
        .map_err(|e| -> Box<dyn std::error::Error> { e })
}
```

- [ ] **Step 6: Build and fix any unused imports**

Run: `cargo build 2>&1`
Expected: clean build. If `std::sync::Arc` in `bridge.rs` or `std::path::Path` in `server.rs` is now unused, remove the import lines and rebuild. The `DAEMON_ADDR` const must compile (it uses const-fn `Ipv4Addr::new`/`SocketAddrV4::new`, stable since 1.69; project floor 1.97).

- [ ] **Step 7: Run clippy**

Run: `cargo clippy --all-targets 2>&1`
Expected: clean except the pre-existing `tests/exec.rs:131` `manual_range_contains` warning. Fix any new warnings (most likely unused imports from the transport swap).

- [ ] **Step 8: Run the test suite**

Run: `cargo test 2>&1`
Expected:
- Lib tests: 17 pass (the 2 `bridge::tests::*` now use TCP; the 2 `daemon::tests::*` now use TCP; `framing::tests::*` unchanged incl. `read_frame_rejects_oversized_length_prefix`; `exec`/`server`/`config` tests unchanged).
- `tests/exec.rs`: 13 pass.
- `tests/daemon_bridge.rs`: ABSENT (deleted in Step 1; recreated in Task 2).
- 0 `#[ignore]` (the 3 old ignored tests are gone).

- [ ] **Step 9: Commit Task 1**

```bash
git add -A
git commit -m "refactor: swap daemon-bridge transport from Unix socket to localhost TCP

The macOS seatbelt sandbox blocks UnixStream::connect (EPERM) even to
allowlisted /tmp/mcp-* paths, defeating the bridge. localhost TCP
connect+bind are both sandbox-permitted (allowLocalBinding: true).

- bridge.rs: RemoteExecutor over TcpStream, DAEMON_ADDR=127.0.0.1:8130
- daemon.rs: TcpListener, drop socket-file perms/stale-cleanup, add
  bound-addr oneshot channel for testability
- server.rs: connect(DAEMON_ADDR)
- cli.rs: pass None for bound-addr channel
- tests/daemon_bridge.rs: deleted (recreated for TCP in next commit)"
```

---

### Task 2: TCP integration tests + docs + mark old plan superseded

Recreate the integration tests for TCP (no longer `#[ignore]` — TCP bind is sandbox-permitted, so they run in the normal suite). Update docs. Mark the old Unix-socket plan/spec superseded.

**Files:**
- Create: `tests/daemon_bridge.rs`
- Modify: `AGENTS.md`
- Modify: `README.md`
- Modify: `docs/superpowers/plans/2026-08-21-daemon-bridge-socket.md` (one-line note)
- Modify: `docs/superpowers/specs/2026-08-21-daemon-bridge-socket-design.md` (one-line note)

**Interfaces:**
- Consumes: `mcp_cli_proxy::bridge::{connect, DaemonOptions}` (now `connect(SocketAddr)`, `DaemonOptions { addr, config }`), `mcp_cli_proxy::daemon::run_daemon` (now `run_daemon(opts, Some(tx))`), `mcp_cli_proxy::exec::{ExecConfig, ExecParams, Executor}`.

- [ ] **Step 1: Create `tests/daemon_bridge.rs` for TCP (no `#[ignore]`)**

Full new file content:

```rust
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};

use mcp_cli_proxy::bridge::{connect, DaemonOptions};
use mcp_cli_proxy::daemon::run_daemon;
use mcp_cli_proxy::exec::{ExecConfig, ExecParams, Executor};

fn ephemeral_addr() -> SocketAddr {
    SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 1), 0))
}

async fn spawn_daemon() -> (tokio::task::JoinHandle<()>, SocketAddr) {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let opts = DaemonOptions {
        addr: ephemeral_addr(),
        config: ExecConfig::defaults(),
    };
    let handle = tokio::spawn(async move {
        let _ = run_daemon(opts, Some(tx)).await;
    });
    let bound = rx.await.expect("daemon did not report bound addr");
    (handle, bound)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_runs_echo_command() {
    let (daemon, addr) = spawn_daemon().await;

    let exec = connect(addr).await.unwrap();
    let params = ExecParams {
        command: "echo hi".into(),
        ..Default::default()
    };
    let result = exec.exec(params).await.unwrap();
    assert_eq!(result.stdout, "hi\n");
    assert_eq!(result.exit_code, Some(0));
    assert!(!result.timed_out);

    daemon.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_reports_nonzero_exit() {
    let (daemon, addr) = spawn_daemon().await;

    let exec = connect(addr).await.unwrap();
    let params = ExecParams {
        command: "exit 7".into(),
        ..Default::default()
    };
    let result = exec.exec(params).await.unwrap();
    assert_eq!(result.exit_code, Some(7));

    daemon.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_enforces_timeout() {
    let (daemon, addr) = spawn_daemon().await;

    let exec = connect(addr).await.unwrap();
    let params = ExecParams {
        command: "sleep 10".into(),
        timeout_secs: Some(1),
        ..Default::default()
    };
    let start = std::time::Instant::now();
    let result = exec.exec(params).await.unwrap();
    assert!(result.timed_out);
    assert_eq!(result.exit_code, None);
    assert!(
        start.elapsed().as_secs() < 5,
        "should be killed near 1s, got {:?}",
        start.elapsed()
    );

    daemon.abort();
}
```

Key points:
- No `#[ignore]` — TCP bind to `127.0.0.1:0` is sandbox-permitted; these run in the normal suite.
- `spawn_daemon()` binds port `0` (ephemeral), receives the actual bound port via the `oneshot` channel, returns the `JoinHandle` + bound addr.
- `daemon.abort()` stops the daemon; the listener is dropped, freeing the port. Each test uses a fresh ephemeral port, so no cross-test conflict.

- [ ] **Step 2: Run the full suite (integration tests now run in-sandbox)**

Run: `cargo test 2>&1`
Expected:
- Lib tests: 17 pass.
- `tests/exec.rs`: 13 pass.
- `tests/daemon_bridge.rs`: 3 pass (echo, nonzero exit, timeout) — NO longer ignored.
- Total: 33 pass, 0 ignored.

If any integration test flakes on the timeout bound, widen the `< 5` to `< 8` (the original `tests/exec.rs:131` uses a similar tolerance). Investigate first; only widen if CI/sandbox timing justifies it.

- [ ] **Step 3: Update `AGENTS.md`**

Read the current `AGENTS.md`. In the "Host commands via proxy" section, replace the socket-path wording with TCP. Replace the `mcp-cli-proxy daemon` prerequisite paragraph block. The updated section should read (keep the rest of the file, including the "Known limitations" section added in commit `29dd372`, intact — just update the transport references):

Change the prerequisite paragraph from:

```
**Prerequisite:** the unsandboxed daemon must be running. Start it in a
separate terminal (not under logoscode) with:

    mcp-cli-proxy daemon

It binds `/tmp/mcp-cli-proxy.sock` (0600) and stays in the foreground. If
`exec_command` fails with "cannot connect to daemon at
/tmp/mcp-cli-proxy.sock (is 'mcp-cli-proxy daemon' running?)", the daemon is
not running — ask the user to start it.
```

to:

```
**Prerequisite:** the unsandboxed daemon must be running. Start it in a
separate terminal (not under logoscode) with:

    mcp-cli-proxy daemon

It listens on `127.0.0.1:8130` and stays in the foreground. If
`exec_command` fails with "cannot connect to daemon at 127.0.0.1:8130
(is 'mcp-cli-proxy daemon' running?)", the daemon is not running — ask
the user to start it.
```

Also update the "Known limitations" section: replace the "No reconnect" bullet's mention of "the dead socket" with "the dead connection", and replace "Unix-only. No Windows named-pipe support; the daemon and bridge use Unix domain sockets." with "Localhost TCP only (127.0.0.1:8130). Any local process can connect (no auth); intended for a personal dev box. Not network-exposed."

- [ ] **Step 4: Update `README.md`**

Read the current `README.md`. Update the transport references in the "Usage" / "Why two processes?" sections and the "Known limitations" section.

In "### 1. Daemon", change:

```
Binds `/tmp/mcp-cli-proxy.sock` (0600) and runs the shell commands the bridge
forwards to it. Start this in a terminal **before** launching the agent
(logoscode) that uses the bridge. It stays in the foreground; Ctrl-C stops it
and removes the socket file.
```

to:

```
Listens on `127.0.0.1:8130` and runs the shell commands the bridge forwards
to it. Start this in a terminal **before** launching the agent (logoscode)
that uses the bridge. It stays in the foreground; Ctrl-C stops it.
```

In "### 2. Bridge / MCP server", change `exits nonzero with a message pointing at 'mcp-cli-proxy daemon'.` — keep it (still accurate).

In "### Why two processes?", change:

```
The agent (e.g. logoscode) sandboxes every process it spawns, including this
one. A sandboxed process cannot run host-level commands (network, `curl`,
`pod install`, ...). The daemon runs outside the sandbox (you start it), so
the shell commands it executes escape the sandbox. The bridge, which the
agent spawns, connects to the daemon over `/tmp/mcp-cli-proxy.sock` — a path
the sandbox's `allowUnixSockets` policy permits.
```

to:

```
The agent (e.g. logoscode) sandboxes every process it spawns, including this
one. A sandboxed process cannot run host-level commands (network, `curl`,
`pod install`, ...). The daemon runs outside the sandbox (you start it), so
the shell commands it executes escape the sandbox. The bridge, which the
agent spawns, connects to the daemon over `127.0.0.1:8130` (localhost TCP) —
the sandbox permits localhost TCP connect/bind (`allowLocalBinding: true`)
but blocks Unix domain sockets.
```

In "## Known limitations", update:
- The "No reconnect" bullet: change "the bridge keeps talking to the dead socket" → "the bridge keeps using the dead connection".
- The "Unix-only" bullet: replace with "Localhost TCP only (127.0.0.1:8130). Any local process can connect (no auth); intended for a personal dev box. Not network-exposed."

- [ ] **Step 5: Mark the old Unix-socket plan superseded**

At the very top of `docs/superpowers/plans/2026-08-21-daemon-bridge-socket.md`, insert (before the first `#` heading):

```
> **SUPERSEDED (2026-08-23):** The Unix-socket transport in this plan is sandbox-blocked (macOS seatbelt blocks `UnixStream::connect` with `EPERM`). The active plan is `2026-08-23-daemon-bridge-tcp.md` (localhost TCP). This file is kept for history.

```

- [ ] **Step 6: Mark the old spec superseded for transport**

At the very top of `docs/superpowers/specs/2026-08-21-daemon-bridge-socket-design.md`, insert (before the first `#` heading):

```
> **TRANSPORT SUPERSEDED (2026-08-23):** The Unix-socket transport described here is sandbox-blocked. The active transport is localhost TCP (see `docs/superpowers/plans/2026-08-23-daemon-bridge-tcp.md`). Non-transport sections (framing, Executor trait, error model) below remain authoritative.

```

- [ ] **Step 7: Final verification**

Run in order:
```bash
cargo build 2>&1
cargo clippy --all-targets 2>&1
cargo test 2>&1
```
Expected:
- build: clean
- clippy: only the pre-existing `tests/exec.rs:131` warning
- test: 17 lib + 13 exec + 3 daemon_bridge = 33 pass, 0 ignored

- [ ] **Step 8: Commit Task 2**

```bash
git add -A
git commit -m "test+docs: TCP integration tests (un-ignored) and transport docs

- tests/daemon_bridge.rs: 3 integration tests now run in-sandbox (TCP
  bind permitted); use ephemeral port + bound-addr oneshot channel
- AGENTS.md/README.md: document 127.0.0.1:8130 transport, localhost
  security note, remove Unix-socket references
- mark 2026-08-21 socket plan/spec superseded for transport"
```

---

## Self-Review

**1. Spec coverage (transport delta only — non-transport spec sections unchanged and already implemented in commits `0b57664..29dd372`):**
- Transport = localhost TCP `127.0.0.1:8130` → Task 1 (`DAEMON_ADDR`) ✓
- Bridge `connect` + loud-fail on daemon-down → Task 1 (`connect` + `BridgeError::DaemonDown`) ✓
- Daemon accept loop + `handle_conn` + SIGINT/SIGTERM → Task 1 (`run_daemon`, `accept_loop`, `handle_conn`) ✓
- Framing unchanged → no task needed (verified: `src/framing.rs` untouched) ✓
- `Executor` trait unchanged → no task needed ✓
- Error model (`DaemonConnectionLost`/`BadResponse`) unchanged → `bridge.rs` rewrite preserves the mapping ✓
- Tests run in-sandbox (no `#[ignore]`) → Task 1 (unit), Task 2 (integration) ✓
- Docs reflect TCP transport → Task 2 ✓
- Old plan/spec marked superseded → Task 2 ✓

**2. Placeholder scan:** No "TBD"/"implement later"/"add error handling". Every code step shows full code. The only discretionary step is Task 2 Step 2's note about widening the timeout tolerance — bounded with a concrete condition and ceiling.

**3. Type consistency:**
- `connect(SocketAddr)` — defined in Task 1 Step 2, used in Task 1 Step 4 (`server.rs`) and Task 2 Step 1 (`tests/daemon_bridge.rs`) ✓
- `DaemonOptions { addr: SocketAddr, config: ExecConfig }` — defined in Task 1 Step 2, used in Task 1 Step 3 (`daemon.rs` via `opts.addr`), Task 1 Step 5 (`cli.rs` via `defaults()`), Task 2 Step 1 (tests) ✓
- `run_daemon(opts, Option<oneshot::Sender<SocketAddr>>)` — defined in Task 1 Step 3, called in Task 1 Step 5 (`cli.rs`, `None`) and Task 2 Step 1 (tests, `Some(tx)`) ✓
- `DAEMON_ADDR: SocketAddr` — defined in Task 1 Step 2, used in Task 1 Step 4 (`server.rs`) and Task 1 Step 2 (`DaemonOptions::defaults()`) ✓
- `RemoteExecutor::from_stream(TcpStream)` — defined and used only in `bridge.rs` tests ✓

**4. Risk check:**
- `DAEMON_ADDR` const: uses `Ipv4Addr::new` + `SocketAddrV4::new` (both const fn, stable 1.69 ≥ project floor 1.97). Low risk. If it somehow fails, fallback is `pub fn daemon_addr() -> SocketAddr { ... }` called at the two use sites — but this is not expected.
- Ephemeral-port test races: each test binds `:0` (unique port), daemon abort drops the listener; no cross-test port reuse. Low risk.
- `oneshot` send happens before `accept_loop` starts; the test receives the addr then connects — the listener is bound and accepting by then (bind is synchronous in `TcpListener::bind`). Actually `bind` is async (`await`); the `local_addr` + send happen immediately after bind resolves, before `accept_loop` is polled in the `select!`. The test's `connect` may race with `accept_loop` registration, but `TcpListener` queues the SYN backlog, so connect succeeds. Low risk.
