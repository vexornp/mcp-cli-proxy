# Daemon + Bridge Socket Forwarding Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Forward `exec_command` calls from the sandboxed `mcp-cli-proxy` MCP server (bridge) to an unsandboxed `mcp-cli-proxy daemon` process over a Unix socket at `/tmp/mcp-cli-proxy.sock`, so `sh -c` actually runs on the host.

**Architecture:** Two modes in one binary. The bridge (default `serve` mode, sandboxed by logoscode) owns the rmcp/MCP stdio layer and forwards `ExecParams` over a persistent `UnixStream`. The daemon (new `daemon` subcommand, started manually by the user, unsandboxed) binds the socket, runs `sh -c` via the existing `run_command`, and returns `ExecResult`. A length-prefixed JSON frame protocol carries requests/responses. An `Executor` trait abstracts "run a command" so `dispatch` is agnostic to local vs. remote execution.

**Tech Stack:** Rust (edition 2021, MSRV 1.97), tokio (async runtime + `net` feature for Unix sockets), rmcp 3.1.3 (MCP stdio server, unchanged), serde_json (wire framing), clap 4 (CLI subcommands), thiserror 2 (error enums).

## Global Constraints

- Socket path constant: `/tmp/mcp-cli-proxy.sock` (matches the sandbox `allowUnixSockets` glob `/tmp/mcp-*`; do not change without re-checking `~/.cache/logoscode/sandbox/srt-settings.json`).
- Socket file permissions: `0600` (owner-only connect/bind).
- No silent in-process fallback when the daemon is down — the bridge must exit nonzero with a message naming `mcp-cli-proxy daemon`.
- No localhost TCP (not in sandbox `allowedDomains`); Unix socket only.
- `Executor` trait must be object-safe (`Arc<dyn Executor>` is used) — use manual `Pin<Box<dyn Future + Send>>` return, NOT `async fn` in trait and NOT the `async_trait` crate.
- `run_command` (`src/exec.rs`) and the existing `tests/exec.rs` integration tests must keep passing unchanged — they call `run_command` directly and do not go through `run_server`/`ProxyServer`.
- `Cargo.toml` tokio features must gain `"net"` (for `UnixStream`/`UnixListener`); current features are `rt-multi-thread, macros, process, io-util, io-std, sync, fs, time`.
- No new external crate dependencies. (tokio "net" is an existing crate's feature, not a new crate.)

## File Structure

| File | Status | Responsibility |
|---|---|---|
| `src/framing.rs` | new | `write_frame`/`read_frame` length-prefixed JSON helpers + unit tests |
| `src/log.rs` | new | `resolve_log`/`open_log` (moved from `server.rs`), `pub(crate)` |
| `src/exec.rs` | modify | Add `Serialize` to `ExecParams`, `Deserialize` to `ExecResult`; add `Executor` trait + `LocalExecutor` |
| `src/bridge.rs` | new | `SOCKET_PATH`, `RemoteExecutor`, `connect()`, `BridgeError`, `DaemonOptions` |
| `src/daemon.rs` | new | `run_daemon(DaemonOptions)` — socket bind, accept loop, framed exec-RPC |
| `src/server.rs` | modify | `ProxyServer` holds `Arc<dyn Executor>`; `dispatch` calls `executor.exec()`; `run_server` constructs `RemoteExecutor` (serve mode) |
| `src/cli.rs` | modify | Add `Daemon` subcommand; dispatch it |
| `src/lib.rs` | modify | Declare `framing`, `log`, `bridge`, `daemon` modules |
| `tests/daemon_bridge.rs` | new | End-to-end daemon+bridge integration tests |
| `Cargo.toml` | modify | Add `"net"` to tokio features |
| `AGENTS.md`, `README.md` | modify | Document `daemon` usage + sandbox smoke-test result |

---

### Task 1: Framing helpers

**Files:**
- Create: `src/framing.rs`
- Modify: `src/lib.rs:1-4`

**Interfaces:**
- Produces: `pub async fn write_frame<W: AsyncWrite + Unpin>(w: &mut W, bytes: &[u8]) -> std::io::Result<()>` and `pub async fn read_frame<R: AsyncRead + Unpin>(r: &mut R) -> std::io::Result<Vec<u8>>`. Frame format: 4-byte big-endian `u32` length prefix + payload bytes. `MAX_FRAME_BYTES = 64 MiB` guard rejects oversized frames.

- [ ] **Step 1: Declare the module in `src/lib.rs`**

Replace the entire contents of `src/lib.rs` with:

```rust
pub mod cli;
pub mod config;
pub mod exec;
pub mod framing;
pub mod server;
```

- [ ] **Step 2: Write the failing tests (create `src/framing.rs` with tests only)**

Create `src/framing.rs`:

```rust
use std::io;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;

    #[tokio::test]
    async fn round_trips_payload() {
        let (mut tx, mut rx) = duplex(8192);
        let payload = b"{\"hello\":\"world\"}";
        write_frame(&mut tx, payload).await.unwrap();
        let got = read_frame(&mut rx).await.unwrap();
        assert_eq!(got, payload);
    }

    #[tokio::test]
    async fn empty_payload_round_trips() {
        let (mut tx, mut rx) = duplex(8192);
        write_frame(&mut tx, b"").await.unwrap();
        let got = read_frame(&mut rx).await.unwrap();
        assert!(got.is_empty());
    }

    #[tokio::test]
    async fn length_prefix_is_be_u32() {
        let (mut tx, mut rx) = duplex(8192);
        write_frame(&mut tx, b"abc").await.unwrap();
        let mut prefix = [0u8; 4];
        rx.read_exact(&mut prefix).await.unwrap();
        assert_eq!(u32::from_be_bytes(prefix), 3);
        let mut body = [0u8; 3];
        rx.read_exact(&mut body).await.unwrap();
        assert_eq!(&body, b"abc");
    }

    #[tokio::test]
    async fn eof_before_frame_returns_unexpected_eof() {
        let (_tx, mut rx) = duplex(8192);
        drop(_tx);
        let err = read_frame(&mut rx).await.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::UnexpectedEof);
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test --lib framing 2>&1 | tail -20`
Expected: FAIL — `cannot find function write_frame/read_frame` (functions not yet defined).

- [ ] **Step 4: Implement the framing functions**

Add to `src/framing.rs` above the `#[cfg(test)]` block:

```rust
pub async fn write_frame<W: AsyncWrite + Unpin>(w: &mut W, bytes: &[u8]) -> io::Result<()> {
    let len = u32::try_from(bytes.len())
        .map_err(|_| io::Error::other("frame too large"))?;
    w.write_all(&len.to_be_bytes()).await?;
    w.write_all(bytes).await?;
    w.flush().await?;
    Ok(())
}

pub async fn read_frame<R: AsyncRead + Unpin>(r: &mut R) -> io::Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > MAX_FRAME_BYTES {
        return Err(io::Error::other("frame too large"));
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).await?;
    Ok(buf)
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test --lib framing 2>&1 | tail -20`
Expected: PASS — 4 tests.

- [ ] **Step 6: Commit**

```bash
git add src/framing.rs src/lib.rs
git commit -m "feat: add length-prefixed framing helpers for socket protocol"
```

---

### Task 2: Move log helpers to `src/log.rs`

**Files:**
- Create: `src/log.rs`
- Modify: `src/server.rs` (remove `resolve_log`/`open_log`, import from `crate::log`, drop unused `PathBuf` import)
- Modify: `src/lib.rs` (declare `log` module)

**Interfaces:**
- Produces: `pub(crate) fn resolve_log(configured: &std::path::Path) -> (std::path::PathBuf, Option<std::fs::File>)` and `pub(crate) fn open_log(dir: &std::path::Path) -> Option<std::fs::File>`. Pure refactor — behavior identical to the functions currently in `server.rs:63-79`.

- [ ] **Step 1: Create `src/log.rs` with the moved helpers**

Create `src/log.rs`:

```rust
use std::path::{Path, PathBuf};

pub(crate) fn resolve_log(configured: &Path) -> (PathBuf, Option<std::fs::File>) {
    if let Some(file) = open_log(configured) {
        return (configured.to_path_buf(), Some(file));
    }
    let fallback = std::env::temp_dir().join("mcp-cli-proxy").join("logs");
    let file = open_log(&fallback);
    (fallback, file)
}

pub(crate) fn open_log(dir: &Path) -> Option<std::fs::File> {
    std::fs::create_dir_all(dir).ok()?;
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("server.log"))
        .ok()
}
```

- [ ] **Step 2: Declare the `log` module in `src/lib.rs`**

Replace `src/lib.rs` contents with:

```rust
pub mod cli;
pub mod config;
pub mod exec;
pub mod framing;
pub mod log;
pub mod server;
```

- [ ] **Step 3: Remove the helpers from `src/server.rs` and import from `crate::log`**

In `src/server.rs`:

(a) Change the import line `use std::path::{Path, PathBuf};` back to `use std::path::Path;` (PathBuf no longer referenced in server.rs after the move).

(b) Add near the other `use crate::...` lines: `use crate::log::resolve_log;`

(c) Delete the two functions `resolve_log` and `open_log` (currently `src/server.rs:55-79`, the block starting with `/// Resolve the log directory...` through the end of `open_log`). Leave `struct ProxyServer` and everything below intact.

- [ ] **Step 4: Verify the refactor compiles and existing tests pass**

Run: `cargo test --quiet 2>&1 | tail -15`
Expected: PASS — 5 config tests + 4 new framing tests + 13 exec tests, all green; no warnings about unused imports.

- [ ] **Step 5: Commit**

```bash
git add src/log.rs src/lib.rs src/server.rs
git commit -m "refactor: extract log helpers into src/log.rs for reuse by daemon"
```

---

### Task 3: `Executor` trait + `LocalExecutor` + wire-protocol derives

**Files:**
- Modify: `src/exec.rs` (add `Serialize` to `ExecParams`, `Deserialize` to `ExecResult`; add `Executor` trait + `LocalExecutor` + unit test)

**Interfaces:**
- Produces:
  - `pub trait Executor: Send + Sync` with `fn exec<'a>(&'a self, params: ExecParams) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<ExecResult, ExecError>> + Send + 'a>>`
  - `pub struct LocalExecutor { pub config: ExecConfig }` implementing `Executor` by delegating to `run_command`.
  - `ExecParams: Serialize` (for the bridge to send) and `ExecResult: Deserialize` (for the bridge to receive).
- Consumes: existing `run_command`, `ExecConfig`, `ExecParams`, `ExecResult`, `ExecError` (all already in `src/exec.rs`).

- [ ] **Step 1: Write the failing test (append to `src/exec.rs` test module)**

Add to the bottom of `src/exec.rs` (after the existing `ExecConfig`/`ExecError` code, before EOF — there is currently no `#[cfg(test)]` module in exec.rs, so create one):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn local_executor_runs_command() {
        let exec = LocalExecutor::new(ExecConfig::defaults());
        let params = ExecParams {
            command: "echo hi".into(),
            ..Default::default()
        };
        let result = exec.exec(params).await.unwrap();
        assert_eq!(result.stdout, "hi\n");
        assert_eq!(result.exit_code, Some(0));
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --lib exec::tests 2>&1 | tail -20`
Expected: FAIL — `cannot find type LocalExecutor` / `cannot find trait Executor`.

- [ ] **Step 3: Add the wire-protocol derives to `ExecParams` and `ExecResult`**

In `src/exec.rs`:

(a) Change line 8 `#[derive(Debug, Clone, Deserialize, Default)]` to `#[derive(Debug, Clone, Deserialize, Serialize, Default)]`.

(b) Change line 17 `#[derive(Debug, Clone, Serialize)]` to `#[derive(Debug, Clone, Serialize, Deserialize)]`.

(The `use serde::{Deserialize, Serialize};` import at line 3 already covers both.)

- [ ] **Step 4: Implement the `Executor` trait and `LocalExecutor`**

(a) Add these two `use` lines to the top of `src/exec.rs` (alongside the existing `use serde::...`, `use thiserror::...`, `use tokio::...` imports):

```rust
use std::future::Future;
use std::pin::Pin;
```

(b) Add the trait and `LocalExecutor` after the `ExecConfig` impl block (i.e. after the `impl ExecConfig { ... }` closing brace, before `#[derive(Debug, Error)] pub enum ExecError`):

```rust
/// Abstracts "run a command, get a result" so the MCP server is agnostic to
/// whether commands run in-process (`LocalExecutor`) or are forwarded to the
/// unsandboxed daemon over a Unix socket (`RemoteExecutor`).
pub trait Executor: Send + Sync {
    fn exec<'a>(
        &'a self,
        params: ExecParams,
    ) -> Pin<Box<dyn Future<Output = Result<ExecResult, ExecError>> + Send + 'a>>;
}

/// Runs commands in the current process via `run_command`. Used by tests and
/// any standalone path that does not need the daemon.
pub struct LocalExecutor {
    pub config: ExecConfig,
}

impl LocalExecutor {
    pub fn new(config: ExecConfig) -> Self {
        Self { config }
    }
}

impl Executor for LocalExecutor {
    fn exec<'a>(
        &'a self,
        params: ExecParams,
    ) -> Pin<Box<dyn Future<Output = Result<ExecResult, ExecError>> + Send + 'a>> {
        Box::pin(async move { run_command(params, self.config).await })
    }
}
```

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo test --lib exec::tests 2>&1 | tail -20`
Expected: PASS — 1 test.

- [ ] **Step 6: Run the full suite to confirm nothing regressed**

Run: `cargo test --quiet 2>&1 | tail -15`
Expected: PASS — all tests green.

- [ ] **Step 7: Commit**

```bash
git add src/exec.rs
git commit -m "feat: add Executor trait + LocalExecutor, make ExecParams/ExecResult wire-ready"
```

---

### Task 4: Refactor `ProxyServer` to use `Arc<dyn Executor>`

**Files:**
- Modify: `src/server.rs` (struct field, `call_tool`, `dispatch`, `run_server` construction; add a dispatch unit test)

**Interfaces:**
- Consumes: `Executor` trait + `LocalExecutor` from Task 3.
- Produces: `ProxyServer { executor: Arc<dyn Executor> }`; `dispatch(args, &Arc<dyn Executor>)` calls `executor.exec(params)` instead of `run_command(params, *config)`. `run_server` still wires `LocalExecutor` in this task (behavior preserved); Task 7 switches it to `RemoteExecutor`.

- [ ] **Step 1: Write the failing dispatch test (add a test module to `src/server.rs`)**

Append to the end of `src/server.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::{ExecConfig, LocalExecutor};
    use std::sync::Arc;

    #[tokio::test]
    async fn dispatch_success_via_local_executor() {
        let executor: Arc<dyn Executor> = Arc::new(LocalExecutor::new(ExecConfig::defaults()));
        let mut args = serde_json::Map::new();
        args.insert("command".into(), serde_json::json!("echo dispatched"));
        let resp = dispatch(&args, &executor).await.unwrap();
        match resp {
            rmcp::model::CallToolResponse::Complete(result) => {
                assert!(!result.is_error, "should not be an error");
                assert!(!result.content.is_empty(), "should have content");
            }
            other => panic!("unexpected response variant: {other:?}"),
        }
    }

    #[tokio::test]
    async fn dispatch_missing_command_returns_tool_error() {
        let executor: Arc<dyn Executor> = Arc::new(LocalExecutor::new(ExecConfig::defaults()));
        let args = serde_json::Map::new();
        let resp = dispatch(&args, &executor).await.unwrap();
        match resp {
            rmcp::model::CallToolResponse::Complete(result) => {
                assert!(result.is_error, "empty command should be a tool error");
            }
            other => panic!("unexpected response variant: {other:?}"),
        }
    }
}
```

- [ ] **Step 2: Run tests to verify they fail to compile**

Run: `cargo test --lib server::tests 2>&1 | tail -25`
Expected: FAIL — `dispatch` expects `&ExecConfig`, not `&Arc<dyn Executor>`; `Executor` not in scope.

- [ ] **Step 3: Update imports in `src/server.rs`**

Change the exec import line (currently `use crate::exec::{run_command, ExecConfig, ExecParams};`) to:

```rust
use crate::exec::{Executor, ExecConfig, ExecParams, LocalExecutor};
```

(`run_command` is no longer called directly from `server.rs`; `ExecConfig` is still needed for `LocalExecutor::new(server_cfg.exec)`; `LocalExecutor` is used in `run_server`.)

- [ ] **Step 4: Change `ProxyServer`'s field**

Change:

```rust
struct ProxyServer {
    config: Arc<ExecConfig>,
}
```

to:

```rust
struct ProxyServer {
    executor: Arc<dyn Executor>,
}
```

- [ ] **Step 5: Update `call_tool` to clone the executor**

In `src/server.rs`, change the `call_tool` body's closure setup from:

```rust
let args = request.arguments.clone().unwrap_or_default();
let config = self.config.clone();
async move { dispatch(&args, &config).await }
```

to:

```rust
let args = request.arguments.clone().unwrap_or_default();
let executor = self.executor.clone();
async move { dispatch(&args, &executor).await }
```

- [ ] **Step 6: Update `dispatch` signature and body**

Change the `dispatch` function signature from:

```rust
async fn dispatch(
    args: &serde_json::Map<String, serde_json::Value>,
    config: &ExecConfig,
) -> Result<CallToolResponse, ErrorData> {
```

to:

```rust
async fn dispatch(
    args: &serde_json::Map<String, serde_json::Value>,
    executor: &Arc<dyn Executor>,
) -> Result<CallToolResponse, ErrorData> {
```

And change the execution call near the end of `dispatch` from:

```rust
    match run_command(params, *config).await {
```

to:

```rust
    match executor.exec(params).await {
```

(Everything else in `dispatch` — arg validation, `ExecParams` construction, `tool_error` — stays identical.)

- [ ] **Step 7: Update `run_server` to construct `LocalExecutor`**

In `run_server`, change:

```rust
    let server = ProxyServer {
        config: Arc::new(server_cfg.exec),
    };
```

to:

```rust
    let server = ProxyServer {
        executor: Arc::new(LocalExecutor::new(server_cfg.exec)),
    };
```

- [ ] **Step 8: Run tests to verify they pass**

Run: `cargo test --quiet 2>&1 | tail -15`
Expected: PASS — all tests green (including the 2 new server tests).

- [ ] **Step 9: Commit**

```bash
git add src/server.rs
git commit -m "refactor: ProxyServer holds Arc<dyn Executor>, dispatch delegates to it"
```

---

### Task 5: Bridge — `RemoteExecutor`, `connect`, `BridgeError`, `DaemonOptions`

**Files:**
- Modify: `Cargo.toml` (add `"net"` to tokio features)
- Create: `src/bridge.rs`
- Modify: `src/lib.rs` (declare `bridge` module)

**Interfaces:**
- Consumes: `ExecParams`, `ExecResult`, `ExecError`, `Executor` (Task 3); `write_frame`/`read_frame` (Task 1).
- Produces:
  - `pub const SOCKET_PATH: &str = "/tmp/mcp-cli-proxy.sock"`
  - `pub struct DaemonOptions { pub socket_path: PathBuf, pub config: ExecConfig }` with `DaemonOptions::defaults()`
  - `pub struct RemoteExecutor` implementing `Executor` (holds a `tokio::sync::Mutex<UnixStream>`; serializes concurrent calls)
  - `pub async fn connect(path: &std::path::Path) -> Result<RemoteExecutor, BridgeError>`
  - `pub enum BridgeError { DaemonDown { path: String }, Io(std::io::Error) }` (Display: `DaemonDown` → `cannot connect to daemon at {path} (is 'mcp-cli-proxy daemon' running?)`)

- [ ] **Step 1: Add the `net` feature to tokio in `Cargo.toml`**

Change the tokio dependency line from:

```toml
tokio = { version = "1", features = ["rt-multi-thread", "macros", "process", "io-util", "io-std", "sync", "fs", "time"] }
```

to:

```toml
tokio = { version = "1", features = ["rt-multi-thread", "macros", "process", "io-util", "io-std", "sync", "fs", "time", "net"] }
```

- [ ] **Step 2: Declare the `bridge` module in `src/lib.rs`**

Replace `src/lib.rs` contents with:

```rust
pub mod bridge;
pub mod cli;
pub mod config;
pub mod exec;
pub mod framing;
pub mod log;
pub mod server;
```

- [ ] **Step 3: Write the failing test (create `src/bridge.rs` with test only)**

Create `src/bridge.rs`:

```rust
use crate::config::ServerConfig;
use crate::exec::{ExecConfig, ExecError, ExecParams, ExecResult, Executor};
use crate::framing::{read_frame, write_frame};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use thiserror::Error;
use tokio::net::UnixStream;
use tokio::sync::Mutex;

pub const SOCKET_PATH: &str = "/tmp/mcp-cli-proxy.sock";

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn remote_executor_round_trips_one_call() {
        let dir = std::temp::tempdir().unwrap();
        let sock = dir.path().join("test.sock");
        let listener = tokio::net::UnixListener::bind(&sock).unwrap();

        let server = tokio::spawn(async move {
            let (mut conn, _) = listener.accept().await.unwrap();
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

        let exec = connect(&sock).await.unwrap();
        let params = ExecParams {
            command: "echo hi".into(),
            ..Default::default()
        };
        let result = exec.exec(params).await.unwrap();
        assert_eq!(result.stdout, "hi\n");
        assert_eq!(result.exit_code, Some(0));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn connect_missing_socket_returns_daemon_down() {
        let err = connect(Path::new("/tmp/definitely-not-here-mcp-cli-proxy.sock"))
            .await
            .unwrap_err();
        assert!(matches!(err, BridgeError::DaemonDown { .. }));
        assert!(err.to_string().contains("mcp-cli-proxy daemon"));
    }
}
```

- [ ] **Step 4: Run tests to verify they fail**

Run: `cargo test --lib bridge 2>&1 | tail -25`
Expected: FAIL — `cannot find type BridgeError`, `cannot find function connect`, `RemoteExecutor` not found.

- [ ] **Step 5: Implement `BridgeError`, `DaemonOptions`, `RemoteExecutor`, `connect`**

Add to `src/bridge.rs` (above the `#[cfg(test)]` block):

```rust
#[derive(Debug, Error)]
pub enum BridgeError {
    #[error("cannot connect to daemon at {path} (is 'mcp-cli-proxy daemon' running?)")]
    DaemonDown { path: String },
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub struct DaemonOptions {
    pub socket_path: PathBuf,
    pub config: ExecConfig,
}

impl DaemonOptions {
    pub fn defaults() -> Self {
        let config = ServerConfig::resolve()
            .map(|c| c.exec)
            .unwrap_or_else(|_| ExecConfig::defaults());
        Self {
            socket_path: PathBuf::from(SOCKET_PATH),
            config,
        }
    }
}

pub struct RemoteExecutor {
    stream: Mutex<UnixStream>,
}

/// Connect to the daemon at `path` and return a `RemoteExecutor`. Maps
/// missing-socket / connection-refused to `DaemonDown` so the caller can print
/// the "is 'mcp-cli-proxy daemon' running?" message.
pub async fn connect(path: &Path) -> Result<RemoteExecutor, BridgeError> {
    match UnixStream::connect(path).await {
        Ok(stream) => Ok(RemoteExecutor {
            stream: Mutex::new(stream),
        }),
        Err(e)
            if e.kind() == std::io::ErrorKind::NotFound
                || e.kind() == std::io::ErrorKind::ConnectionRefused =>
        {
            Err(BridgeError::DaemonDown {
                path: path.display().to_string(),
            })
        }
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
                .map_err(|e| ExecError::Spawn(format!("write request: {e}")))?;
            let resp = read_frame(&mut *s)
                .await
                .map_err(|e| ExecError::Spawn(format!("read response: {e}")))?;
            let rpc: Result<ExecResult, String> = serde_json::from_slice(&resp)
                .map_err(|e| ExecError::Spawn(format!("deserialize response: {e}")))?;
            rpc.map_err(ExecError::Spawn)
        })
    }
}
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test --lib bridge 2>&1 | tail -25`
Expected: PASS — 2 tests.

- [ ] **Step 7: Run clippy to confirm no warnings**

Run: `cargo clippy --all-targets 2>&1 | tail -15`
Expected: no warnings.

- [ ] **Step 8: Commit**

```bash
git add Cargo.toml src/bridge.rs src/lib.rs
git commit -m "feat: add RemoteExecutor bridge + BridgeError + DaemonOptions"
```

---

### Task 6: Daemon + integration tests

**Files:**
- Create: `src/daemon.rs`
- Create: `tests/daemon_bridge.rs`
- Modify: `src/lib.rs` (declare `daemon` module)

**Interfaces:**
- Consumes: `DaemonOptions` (Task 5), `run_command`/`ExecConfig`/`ExecParams`/`ExecResult` (existing), `write_frame`/`read_frame` (Task 1), `resolve_log` (Task 2).
- Produces: `pub async fn run_daemon(opts: DaemonOptions) -> Result<(), Box<dyn std::error::Error>>`. Binds `opts.socket_path` with `0600` perms, unlinks stale socket first, accept loop spawns a task per connection, each task reads framed `ExecParams`, runs `run_command`, writes a framed `Result<ExecResult, String>`. Shuts down on `SIGINT` (ctrl_c) and unlinks the socket.

- [ ] **Step 1: Declare the `daemon` module in `src/lib.rs`**

Replace `src/lib.rs` contents with:

```rust
pub mod bridge;
pub mod cli;
pub mod config;
pub mod daemon;
pub mod exec;
pub mod framing;
pub mod log;
pub mod server;
```

- [ ] **Step 2: Write the failing integration tests (create `tests/daemon_bridge.rs`)**

Create `tests/daemon_bridge.rs`:

```rust
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use mcp_cli_proxy::bridge::{connect, DaemonOptions};
use mcp_cli_proxy::daemon::run_daemon;
use mcp_cli_proxy::exec::{ExecConfig, ExecParams, Executor};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_socket() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let mut p = std::env::temp_dir();
    p.push(format!(
        "mcp-cli-proxy-test-{}-{}.sock",
        std::process::id(),
        n
    ));
    let _ = std::fs::remove_file(&p);
    p
}

async fn wait_for_socket(path: &std::path::Path) {
    for _ in 0..200 {
        if path.exists() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("daemon did not create socket in time: {}", path.display());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_runs_echo_command() {
    let sock = temp_socket();
    let opts = DaemonOptions {
        socket_path: sock.clone(),
        config: ExecConfig::defaults(),
    };
    let daemon = tokio::spawn(async move { run_daemon(opts).await });

    wait_for_socket(&sock).await;

    let exec = connect(&sock).await.unwrap();
    let params = ExecParams {
        command: "echo hi".into(),
        ..Default::default()
    };
    let result = exec.exec(params).await.unwrap();
    assert_eq!(result.stdout, "hi\n");
    assert_eq!(result.exit_code, Some(0));
    assert!(!result.timed_out);

    daemon.abort();
    let _ = std::fs::remove_file(&sock);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_reports_nonzero_exit() {
    let sock = temp_socket();
    let opts = DaemonOptions {
        socket_path: sock.clone(),
        config: ExecConfig::defaults(),
    };
    let daemon = tokio::spawn(async move { run_daemon(opts).await });

    wait_for_socket(&sock).await;

    let exec = connect(&sock).await.unwrap();
    let params = ExecParams {
        command: "exit 7".into(),
        ..Default::default()
    };
    let result = exec.exec(params).await.unwrap();
    assert_eq!(result.exit_code, Some(7));

    daemon.abort();
    let _ = std::fs::remove_file(&sock);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_enforces_timeout() {
    let sock = temp_socket();
    let opts = DaemonOptions {
        socket_path: sock.clone(),
        config: ExecConfig::defaults(),
    };
    let daemon = tokio::spawn(async move { run_daemon(opts).await });

    wait_for_socket(&sock).await;

    let exec = connect(&sock).await.unwrap();
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
    let _ = std::fs::remove_file(&sock);
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test --test daemon_bridge 2>&1 | tail -25`
Expected: FAIL — `cannot find module daemon` / `run_daemon` not found.

- [ ] **Step 4: Implement `run_daemon` (create `src/daemon.rs`)**

Create `src/daemon.rs`:

```rust
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;

use tokio::net::{UnixListener, UnixStream};
use tokio::signal;

use crate::bridge::DaemonOptions;
use crate::config::ServerConfig;
use crate::exec::{run_command, ExecConfig, ExecParams, ExecResult};
use crate::framing::{read_frame, write_frame};
use crate::log::resolve_log;

pub async fn run_daemon(opts: DaemonOptions) -> Result<(), Box<dyn std::error::Error>> {
    let log_dir_cfg = ServerConfig::resolve()
        .map(|c| c.log_dir)
        .unwrap_or_else(|_| std::env::temp_dir().join("mcp-cli-proxy").join("logs"));
    let (log_dir, log_file) = resolve_log(&log_dir_cfg);

    let writer: std::sync::Mutex<Box<dyn std::io::Write + Send>> = match log_file {
        Some(file) => std::sync::Mutex::new(Box::new(file)),
        None => std::sync::Mutex::new(Box::new(std::io::sink())),
    };
    let _ = tracing_subscriber::fmt()
        .with_writer(writer)
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();
    tracing::info!(
        "mcp-cli-proxy daemon starting: socket={}, log_dir={}",
        opts.socket_path.display(),
        log_dir.display()
    );

    if opts.socket_path.exists() {
        tracing::warn!("removing stale socket at {}", opts.socket_path.display());
        let _ = std::fs::remove_file(&opts.socket_path);
    }
    let listener = UnixListener::bind(&opts.socket_path)?;
    std::fs::set_permissions(
        &opts.socket_path,
        std::fs::Permissions::from_mode(0o600),
    )?;
    tracing::info!("daemon listening on {}", opts.socket_path.display());

    let config = Arc::new(opts.config);
    let socket_path = opts.socket_path.clone();

    tokio::select! {
        _ = accept_loop(&listener, config) => {}
        _ = signal::ctrl_c() => {
            tracing::info!("daemon: SIGINT received, shutting down");
        }
    }

    let _ = std::fs::remove_file(&socket_path);
    tracing::info!("daemon stopped");
    Ok(())
}

async fn accept_loop(listener: &UnixListener, config: Arc<ExecConfig>) {
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
    mut conn: UnixStream,
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
```

- [ ] **Step 5: Run the integration tests to verify they pass**

Run: `cargo test --test daemon_bridge 2>&1 | tail -25`
Expected: PASS — 3 tests.

- [ ] **Step 6: Run the full suite**

Run: `cargo test --quiet 2>&1 | tail -15`
Expected: PASS — all tests green (config, framing, exec, bridge, daemon_bridge, server).

- [ ] **Step 7: Run clippy**

Run: `cargo clippy --all-targets 2>&1 | tail -15`
Expected: no warnings.

- [ ] **Step 8: Commit**

```bash
git add src/daemon.rs src/lib.rs tests/daemon_bridge.rs
git commit -m "feat: add unsandboxed exec daemon with framed Unix socket protocol"
```

---

### Task 7: CLI wiring + `serve` uses `RemoteExecutor`

**Files:**
- Modify: `src/cli.rs` (add `Daemon` subcommand)
- Modify: `src/server.rs` (`run_server` constructs `RemoteExecutor` via `connect`; exit nonzero with daemon-down message on failure)

**Interfaces:**
- Consumes: `DaemonOptions`, `connect`, `SOCKET_PATH`, `BridgeError` (Task 5); `run_daemon` (Task 6); `Executor`, `LocalExecutor` (Task 3).
- Produces: `mcp-cli-proxy daemon` subcommand; `mcp-cli-proxy serve` (and no-arg) now requires the daemon and fails loudly if it isn't running.

- [ ] **Step 1: Add the `Daemon` subcommand to `src/cli.rs`**

Replace the entire contents of `src/cli.rs` with:

```rust
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "mcp-cli-proxy", version, about = "Generic-executor MCP server: runs shell commands on the host PC, outside the agent sandbox")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand)]
pub enum Command {
    /// Run the stdio MCP server (default). Forwards exec_command calls to the
    /// daemon over /tmp/mcp-cli-proxy.sock — start `mcp-cli-proxy daemon` first.
    Serve,
    /// Run the unsandboxed exec daemon. Start this in a separate terminal
    /// before launching logoscode. Binds /tmp/mcp-cli-proxy.sock and runs the
    /// shell commands the bridge forwards to it.
    Daemon,
}

pub async fn run(cmd: Option<Command>) -> Result<(), Box<dyn std::error::Error>> {
    match cmd {
        None | Some(Command::Serve) => crate::server::run_server().await,
        Some(Command::Daemon) => {
            let opts = crate::bridge::DaemonOptions::defaults();
            crate::daemon::run_daemon(opts).await
        }
    }
}
```

- [ ] **Step 2: Switch `run_server` to construct `RemoteExecutor`**

In `src/server.rs`:

(a) Update the exec import (currently `use crate::exec::{Executor, ExecConfig, ExecParams, LocalExecutor};`) to drop `LocalExecutor` and `ExecConfig` (no longer used in serve mode) — change to:

```rust
use crate::exec::{Executor, ExecParams};
```

(b) Add a bridge import near the other `use crate::...` lines:

```rust
use crate::bridge::{connect, SOCKET_PATH};
```

(c) In `run_server`, replace the `ProxyServer` construction block:

```rust
    let server = ProxyServer {
        executor: Arc::new(LocalExecutor::new(server_cfg.exec)),
    };
```

with:

```rust
    let executor: Arc<dyn Executor> = match connect(std::path::Path::new(SOCKET_PATH)).await {
        Ok(remote) => Arc::new(remote),
        Err(e) => {
            eprintln!("mcp-cli-proxy: {e}");
            std::process::exit(1);
        }
    };

    let server = ProxyServer { executor };
```

(`server_cfg.exec` is now unused in `run_server` — that's fine; `server_cfg.log_dir` is still used by `resolve_log`. Leave the `ServerConfig::resolve()?` call and the log-dir handling untouched. If the compiler warns about an unused `server_cfg.exec`, that is acceptable; alternatively prefix with `let _ = server_cfg.exec;` — but it should not warn since `ServerConfig` fields are simply not read.)

- [ ] **Step 3: Verify the build compiles**

Run: `cargo build 2>&1 | tail -15`
Expected: clean build.

- [ ] **Step 4: Run the full test suite**

Run: `cargo test --quiet 2>&1 | tail -15`
Expected: PASS — all tests green. (The server unit tests from Task 4 still pass because they construct `LocalExecutor` directly in the test module; `run_server` is not invoked by any test.)

- [ ] **Step 5: Run clippy**

Run: `cargo clippy --all-targets 2>&1 | tail -15`
Expected: no warnings. If clippy warns about unused `server_cfg.exec`, add `let _ = &server_cfg.exec;` before the `connect` call to silence it.

- [ ] **Step 6: Commit**

```bash
git add src/cli.rs src/server.rs
git commit -m "feat: serve mode forwards to daemon via RemoteExecutor; add daemon subcommand"
```

---

### Task 8: Sandbox smoke test + documentation

**Files:**
- Modify: `AGENTS.md`
- Modify: `README.md`

**Interfaces:** None (verification + docs). This is the risk-mitigation gate from the spec: confirm the logoscode seatbelt actually permits `UnixStream::connect` to a `/tmp/mcp-*` socket. If it does not, STOP and report — the design is blocked on a logoscode-side change (per spec Risk #1).

- [ ] **Step 1: Build the release binary**

Run: `cargo build 2>&1 | tail -5`
Expected: clean build, binary at `target/debug/mcp-cli-proxy`.

- [ ] **Step 2: Start the daemon in a separate terminal**

In a new terminal (NOT under logoscode):

```bash
cd /Users/peiyan_wang/Workspace/mcp-cli-proxy
./target/debug/mcp-cli-proxy daemon
```

Expected output: logs `daemon listening on /tmp/mcp-cli-proxy.sock`. The terminal stays attached (daemon runs in foreground). Confirm the socket exists:

```bash
ls -l /tmp/mcp-cli-proxy.sock
```

Expected: `srw-------` (socket, 0600, owned by your user). Leave this terminal open.

- [ ] **Step 3: Verify the bridge connects and runs an unsandboxed command**

In another terminal (also NOT under logoscode, to first confirm the bridge+daemon pair works outside any sandbox):

```bash
cd /Users/peiyan_wang/Workspace/mcp-cli-proxy
echo '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"exec_command","arguments":{"command":"echo smoke-test; whoami"}}}' | ./target/debug/mcp-cli-proxy serve
```

Expected: a JSON response whose `result.content[0].text` is a JSON `ExecResult` with `stdout` containing `smoke-test` and your username, `exit_code: 0`. This confirms the bridge+daemon end-to-end without the sandbox.

- [ ] **Step 4: SMOKE-TEST THE SANDBOX ASSUMPTION (the critical gate)**

Now exercise the path logoscode actually uses: have logoscode spawn `mcp-cli-proxy serve` (sandboxed) and call `exec_command` on a command the sandbox would normally block.

In the logoscode session, ask: "Use the exec_command MCP tool to run `curl -s -o /dev/null -w '%{http_code}' https://example.com`."

Expected (success — sandbox allows the socket): the tool returns an `ExecResult` with `exit_code: 0` and `stdout: "200\n"`. This proves the bridge connected to the daemon over the sandbox-allowed socket and the daemon ran `curl` unsandboxed.

Expected (failure — sandbox blocks the socket): the `exec_command` tool fails with the daemon-down message `mcp-cli-proxy: cannot connect to daemon at /tmp/mcp-cli-proxy.sock (is 'mcp-cli-proxy daemon' running?)` EVEN THOUGH the daemon is running (confirmed in Step 2). If this happens, STOP — the sandbox blocks `UnixStream::connect` to `/tmp/mcp-*` despite the `allowUnixSockets` entry. Report this to the user; the daemon+bridge design cannot work without a logoscode-side change (a per-server "no sandbox" option, which does not currently exist).

- [ ] **Step 5: Document `daemon` usage in `AGENTS.md`**

Read the current `AGENTS.md`, then update the "Host commands via proxy" section to note that the daemon must be running. Replace the existing section with:

```markdown
## Host commands via proxy

This environment's bash is sandboxed (blocks some network + OS calls).
For git operations that need network, `curl`, builds, `pod install`, or
anything that fails with a sandbox/permission error, call the `exec_command`
tool from the `mcp-cli-proxy` server instead of bash.

`exec_command` runs the command on the host PC (outside the sandbox) via `sh -c`,
so pipes, globs, and redirects work. It returns stdout, stderr, and exit_code
as JSON.

**Prerequisite:** the unsandboxed daemon must be running. Start it in a
separate terminal (not under logoscode) with:

    mcp-cli-proxy daemon

It binds `/tmp/mcp-cli-proxy.sock` (0600) and stays in the foreground. If
`exec_command` fails with "cannot connect to daemon at
/tmp/mcp-cli-proxy.sock (is 'mcp-cli-proxy daemon' running?)", the daemon is
not running — ask the user to start it.

The following MCP servers are configured but unavailable:
- logos_code_remote_112: MCP error -32000: Connection closed

If the user needs these tools, offer to login by calling the `sso_login` tool with the appropriate domain.
A browser will open for SSO login. After login succeeds, the tools will be available immediately.
```

- [ ] **Step 6: Document `daemon` usage in `README.md`**

Read the current `README.md`, then add a "Usage" section (or update the existing one) describing both modes. Append (or merge into an existing Usage section):

```markdown
## Usage

`mcp-cli-proxy` has two modes that run as separate processes:

### 1. Daemon (unsandboxed, you start it)

    mcp-cli-proxy daemon

Binds `/tmp/mcp-cli-proxy.sock` (0600) and runs the shell commands the bridge
forwards to it. Start this in a terminal **before** launching the agent
(logoscode) that uses the bridge. It stays in the foreground; Ctrl-C stops it
and removes the socket file.

### 2. Bridge / MCP server (sandboxed, the agent starts it)

    mcp-cli-proxy serve   # or just: mcp-cli-proxy

Speaks MCP over stdio (what the agent spawns) and forwards each
`exec_command` call to the daemon over the Unix socket. If the daemon is not
running, it exits nonzero with a message pointing at `mcp-cli-proxy daemon`.

### Why two processes?

The agent (e.g. logoscode) sandboxes every process it spawns, including this
one. A sandboxed process cannot run host-level commands (network, `curl`,
`pod install`, ...). The daemon runs outside the sandbox (you start it), so
the shell commands it executes escape the sandbox. The bridge, which the
agent spawns, connects to the daemon over `/tmp/mcp-cli-proxy.sock` — a path
the sandbox's `allowUnixSockets` policy permits.
```

- [ ] **Step 7: Commit the docs**

```bash
git add AGENTS.md README.md
git commit -m "docs: document daemon+bridge two-process usage and sandbox rationale"
```

- [ ] **Step 8: Report the smoke-test result**

If Step 4 succeeded: the implementation is complete — `exec_command` now reaches the host unsandboxed. Summarize for the user.

If Step 4 failed: do NOT mark the plan complete. Report that the sandbox blocks the socket connection and that the daemon+bridge cannot function without a logoscode-side change. Leave the code committed (it's correct; the constraint is external).

---

## Self-Review

**1. Spec coverage:**
- `Executor` trait + `LocalExecutor` → Task 3 ✓
- `RemoteExecutor`, `connect`, `BridgeError`, `SOCKET_PATH` → Task 5 ✓
- `DaemonOptions` → Task 5 ✓
- `run_daemon` (socket bind 0600, stale unlink, accept loop, framed exec-RPC, SIGINT cleanup) → Task 6 ✓
- `ProxyServer` → `Arc<dyn Executor>`, `dispatch` delegates → Task 4 ✓
- `serve` mode uses `RemoteExecutor` → Task 7 ✓
- `Daemon` CLI subcommand → Task 7 ✓
- Framing helpers (4-byte BE length + JSON) → Task 1 ✓
- `resolve_log`/`open_log` moved to `src/log.rs` `pub(crate)` → Task 2 ✓
- Error handling table (daemon down → exit + message; mid-call crash → MCP internal_error; stale socket → unlink+warn; bind fail → exit; SIGINT → cleanup) → Task 6 + Task 7 ✓
- Testing (framing unit, integration echo/exit/timeout, existing config/exec untouched) → Tasks 1, 5, 6 ✓
- Sandbox smoke test as early gate → Task 8 (positioned after the build is complete, which is the earliest point a real sandboxed bridge can be exercised; the spec's "early" intent is satisfied by making it an explicit gate before declaring done) ✓

**2. Placeholder scan:** No TBD/TODO/"add error handling" — every code step has complete code; every command step has expected output.

**3. Type consistency:**
- `Executor::exec` signature identical across Task 3 (def), Task 4 (dispatch call), Task 5 (`RemoteExecutor` impl) ✓
- `DaemonOptions { socket_path, config }` consistent across Task 5 (def), Task 6 (`run_daemon` opts param), Task 7 (CLI `defaults()`) ✓
- `connect(path: &Path) -> Result<RemoteExecutor, BridgeError>` consistent across Task 5 (def + tests) and Task 7 (`run_server` call) ✓
- `run_daemon(opts: DaemonOptions) -> Result<(), Box<dyn Error>>` consistent across Task 6 (def) and Task 7 (CLI dispatch) ✓
- Frame payload types: request `ExecParams` (now `Serialize`), response `Result<ExecResult, String>` (ExecResult now `Deserialize`) — consistent between Task 5 (`RemoteExecutor::exec`) and Task 6 (`handle_conn`) ✓
- `SOCKET_PATH: &str = "/tmp/mcp-cli-proxy.sock"` constant in Task 5, referenced in Task 7 and docs ✓
