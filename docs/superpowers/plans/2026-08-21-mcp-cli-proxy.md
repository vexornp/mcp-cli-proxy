# mcp-cli-proxy Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a generic-executor MCP server (`mcp-cli-proxy`) that runs arbitrary shell commands on the host PC via a single `exec_command` tool, bypassing the agent's sandbox.

**Architecture:** Single Rust crate with both a library target (for integration tests) and a binary target. The `exec` module holds all execution logic with zero `rmcp` dependencies, making it independently testable. The `server` module is a thin rmcp `ServerHandler` shell that deserializes tool args, calls `exec::run_command`, and serializes the result. stdio transport, matching the user's `xcode-mcp` conventions.

**Tech Stack:** Rust 2021 (rust-version 1.97), `rmcp` 3.1.3 (MCP SDK, stdio transport), `tokio` (async runtime + process), `serde`/`serde_json`, `thiserror`, `clap` (CLI), `tracing`/`tracing-subscriber` (logging), `libc` (process-group signal).

## Global Constraints

Copied verbatim from the design spec (`docs/superpowers/specs/2026-08-21-mcp-cli-proxy-design.md`):

- **Edition:** Rust 2021, `rust-version = "1.97"`.
- **Dependencies (exact):** `rmcp = { version = "3.1.3", features = ["transport-io"] }`; `tokio = { version = "1", features = ["rt-multi-thread", "macros", "process", "io-util", "io-std", "sync", "fs", "time"] }`; `serde = { version = "1", features = ["derive"] }`; `serde_json = "1"`; `thiserror = "2"`; `tracing = "0.1"`; `tracing-subscriber = { version = "0.3", features = ["env-filter"] }`; `clap = { version = "4", features = ["derive"] }`; `libc = "0.2"`.
- **Transport:** stdio only (local subprocess). No network transport.
- **Tool surface:** exactly one tool named `exec_command`. No other tools.
- **Security model:** unrestricted — any command, any cwd. No allowlist, no cwd root. The only guard is a per-call timeout (robustness, not a security gate).
- **Execution model:** commands run via `sh -c "<command>"` (shell string), supporting pipes/globs/redirects.
- **Result handling:** inline `stdout`/`stderr`/`exit_code`, each stream truncated independently at `output_cap_bytes` (default 102400). No file spilling.
- **Error layering:** tool-level input errors (empty command, bad cwd, `timeout_secs <= 0`) → MCP tool error result (`isError: true`). Internal failures (spawn/IO) → `ErrorData` (protocol error).
- **`exit_code` semantics:** `null` **only** when `timed_out` is true. Always an integer otherwise (including 127 for command-not-found).
- **Timeout:** default 120s, hard max 1800s. `timeout_secs` arg is clamped to max (not rejected). On timeout, kill the **whole process group** (via `libc::kill(-pid, SIGKILL)`), reap, return partial output captured so far with `timed_out=true`.
- **Config resolution order:** (1) config file at `$XDG_CONFIG_HOME/mcp-cli-proxy/config` (or `~/.config/mcp-cli-proxy/config`), line-based `key = value`, `#` comments; (2) environment variables override file. Keys: `output_cap_bytes`, `default_timeout_secs`, `max_timeout_secs`, `log_dir`. Env vars: `MCP_CLI_PROXY_OUTPUT_CAP`, `MCP_CLI_PROXY_DEFAULT_TIMEOUT`, `MCP_CLI_PROXY_MAX_TIMEOUT`, `MCP_CLI_PROXY_LOG_DIR`.
- **Project structure:** single crate `mcp-cli-proxy` with `src/lib.rs` (declares `pub mod` for each module, enables integration tests) + `src/main.rs` (thin binary) + `src/{cli,exec,config,server}.rs` + `tests/exec.rs`.
- **Conventions:** mirror the user's `XcodeMcp` project (clap CLI with optional `serve` subcommand defaulting to serve, tracing logs to `<log_dir>/server.log`, `ServerHandler` impl shape, `resolve`-style config).

---

## File Structure

| File | Responsibility | Created in |
|---|---|---|
| `Cargo.toml` | Package + all dependencies. | Task 1 |
| `src/lib.rs` | Crate root; declares `pub mod cli/config/exec/server`. Enables integration tests. | Task 1 |
| `src/main.rs` | Thin binary: parses CLI, calls `cli::run`. | Task 1 |
| `src/cli.rs` | clap `Cli` + `Command::Serve` (default), `run()` dispatches to `server::run_server`. | Task 1 |
| `src/server.rs` | rmcp `ServerHandler` impl, tool list, `dispatch`, validation, tracing setup. Stub in Task 1; full impl in Task 5. | Task 1 (stub), Task 5 (full) |
| `src/config.rs` | `ServerConfig`, `ServerConfig::resolve()`, `parse_kv()`, path helpers; re-exports `ExecConfig` from `exec.rs`. | Task 4 (full) |
| `src/exec.rs` | `ExecParams`, `ExecResult`, `ExecError`, `run_command()`, `drain()`, `kill_group()`. No `rmcp` imports. | Task 1 (basic), Task 2 (inputs), Task 3 (limits) |
| `tests/exec.rs` | Integration tests exercising `run_command` directly. | Tasks 1–3 |
| `README.md` | Overview, install, config, host registration, smoke test. | Task 6 |
| `AGENTS.md` | Drop-in agent instructions snippet for routing to the proxy. | Task 6 |

---

## Task 1: Scaffold + exec.rs happy path

**Files:**
- Create: `Cargo.toml`
- Create: `src/lib.rs`, `src/main.rs`, `src/cli.rs`, `src/server.rs` (stub), `src/config.rs` (stub), `src/exec.rs`
- Create: `tests/exec.rs`

**Interfaces:**
- Produces: `pub struct ExecParams { command, cwd, env, timeout_secs, stdin }` (derives `Debug, Clone, Deserialize, Default`); `pub struct ExecResult { exit_code, stdout, stderr, stdout_truncated, stderr_truncated, timed_out, duration_ms }` (derives `Debug, Clone, Serialize`); `pub struct ExecConfig { output_cap_bytes, default_timeout_secs, max_timeout_secs }` with `ExecConfig::defaults()`; `pub enum ExecError` (`Spawn(String)`, `Io(std::io::Error)`); `pub async fn run_command(params: ExecParams, config: ExecConfig) -> Result<ExecResult, ExecError>`. Later tasks consume these exact names.

- [ ] **Step 1: Create `Cargo.toml`**

```toml
[package]
name = "mcp-cli-proxy"
edition = "2021"
rust-version = "1.97"
version = "0.1.0"
license = "MIT"

[dependencies]
rmcp = { version = "3.1.3", features = ["transport-io"] }
tokio = { version = "1", features = ["rt-multi-thread", "macros", "process", "io-util", "io-std", "sync", "fs", "time"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
thiserror = "2"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
clap = { version = "4", features = ["derive"] }
libc = "0.2"
```

- [ ] **Step 2: Create `src/lib.rs`**

```rust
pub mod cli;
pub mod config;
pub mod exec;
pub mod server;
```

- [ ] **Step 3: Create `src/main.rs`**

```rust
use clap::Parser;
use mcp_cli_proxy::cli::{Cli, run};

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    if let Err(e) = run(cli.command).await {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
```

- [ ] **Step 4: Create `src/cli.rs`**

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
    /// Run the stdio MCP server (default).
    Serve,
}

pub async fn run(cmd: Option<Command>) -> Result<(), Box<dyn std::error::Error>> {
    match cmd {
        None | Some(Command::Serve) => crate::server::run_server().await,
    }
}
```

- [ ] **Step 5: Create `src/server.rs` stub**

```rust
// Full rmcp ServerHandler impl added in Task 5.
pub async fn run_server() -> Result<(), Box<dyn std::error::Error>> {
    Ok(())
}
```

- [ ] **Step 6: Create `src/config.rs` stub**

```rust
// ExecConfig + ServerConfig::resolve() added in Task 4.
```

- [ ] **Step 7: Create `src/exec.rs` with types + basic `run_command`**

```rust
use std::collections::HashMap;
use std::time::{Duration, Instant};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::process::Command;

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ExecParams {
    pub command: String,
    pub cwd: Option<String>,
    pub env: Option<HashMap<String, String>>,
    pub timeout_secs: Option<u64>,
    pub stdin: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExecResult {
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub timed_out: bool,
    pub duration_ms: u128,
}

#[derive(Debug, Clone, Copy)]
pub struct ExecConfig {
    pub output_cap_bytes: usize,
    pub default_timeout_secs: u64,
    pub max_timeout_secs: u64,
}

impl ExecConfig {
    pub fn defaults() -> Self {
        Self {
            output_cap_bytes: 102_400,
            default_timeout_secs: 120,
            max_timeout_secs: 1800,
        }
    }
}

#[derive(Debug, Error)]
pub enum ExecError {
    #[error("spawn failed: {0}")]
    Spawn(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub async fn run_command(params: ExecParams, config: ExecConfig) -> Result<ExecResult, ExecError> {
    let start = Instant::now();
    let timeout = Duration::from_secs(config.default_timeout_secs);

    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(&params.command);
    cmd.kill_on_drop(true);
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let mut child = cmd.spawn().map_err(|e| ExecError::Spawn(e.to_string()))?;

    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(output)) => Ok(ExecResult {
            exit_code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            stdout_truncated: false,
            stderr_truncated: false,
            timed_out: false,
            duration_ms: start.elapsed().as_millis(),
        }),
        Ok(Err(e)) => Err(ExecError::Io(e)),
        Err(_) => Ok(ExecResult {
            exit_code: None,
            stdout: String::new(),
            stderr: String::new(),
            stdout_truncated: false,
            stderr_truncated: false,
            timed_out: true,
            duration_ms: start.elapsed().as_millis(),
        }),
    }
}
```

- [ ] **Step 8: Write the failing tests in `tests/exec.rs`**

```rust
use mcp_cli_proxy::exec::{run_command, ExecConfig, ExecParams};

#[tokio::test]
async fn runs_simple_command() {
    let params = ExecParams {
        command: "echo hello".into(),
        ..Default::default()
    };
    let result = run_command(params, ExecConfig::defaults()).await.unwrap();
    assert_eq!(result.exit_code, Some(0));
    assert_eq!(result.stdout, "hello\n");
    assert!(!result.timed_out);
}

#[tokio::test]
async fn captures_stderr_separately() {
    let params = ExecParams {
        command: "echo out; echo err >&2".into(),
        ..Default::default()
    };
    let result = run_command(params, ExecConfig::defaults()).await.unwrap();
    assert_eq!(result.stdout, "out\n");
    assert_eq!(result.stderr, "err\n");
}

#[tokio::test]
async fn nonzero_exit() {
    let params = ExecParams {
        command: "false".into(),
        ..Default::default()
    };
    let result = run_command(params, ExecConfig::defaults()).await.unwrap();
    assert_eq!(result.exit_code, Some(1));
}

#[tokio::test]
async fn command_not_found_exit_code() {
    let params = ExecParams {
        command: "this_does_not_exist_xyz".into(),
        ..Default::default()
    };
    let result = run_command(params, ExecConfig::defaults()).await.unwrap();
    assert_eq!(result.exit_code, Some(127));
}

#[tokio::test]
async fn pipes_and_globs_work() {
    // sh -c handles pipes natively; confirms shell-string execution model.
    let params = ExecParams {
        command: "printf 'a\nb\nc\n' | grep a".into(),
        ..Default::default()
    };
    let result = run_command(params, ExecConfig::defaults()).await.unwrap();
    assert_eq!(result.stdout, "a\n");
}
```

- [ ] **Step 9: Run tests to verify they pass**

Run: `cargo test`
Expected: 5 tests pass.

- [ ] **Step 10: Verify the binary builds and `--help` works**

Run: `cargo run -- --help`
Expected: prints clap help mentioning `mcp-cli-proxy` and the `serve` subcommand, exits 0.

- [ ] **Step 11: Commit**

```bash
git add Cargo.toml src/ tests/
git commit -m "feat: scaffold mcp-cli-proxy crate + exec_command happy path"
```

---

## Task 2: exec.rs inputs — stdin, cwd, env

**Files:**
- Modify: `src/exec.rs` (rewrite `run_command` to pipe stdin + set cwd/env)
- Modify: `tests/exec.rs` (add 3 tests)

**Interfaces:**
- Consumes: `ExecParams` fields `cwd`, `env`, `stdin` (already declared in Task 1 but unused).
- Produces: no signature change. `run_command` now honors all `ExecParams` fields except `timeout_secs` (still uses `config.default_timeout_secs`; per-arg timeout + truncation come in Task 3).

- [ ] **Step 1: Write the failing tests (append to `tests/exec.rs`)**

```rust
use std::collections::HashMap;
use std::path::Path;

#[tokio::test]
async fn pipes_stdin() {
    let params = ExecParams {
        command: "cat".into(),
        stdin: Some("hi\n".into()),
        ..Default::default()
    };
    let result = run_command(params, ExecConfig::defaults()).await.unwrap();
    assert_eq!(result.stdout, "hi\n");
}

#[tokio::test]
async fn respects_cwd() {
    let cwd = std::env::temp_dir();
    let params = ExecParams {
        command: "pwd".into(),
        cwd: Some(cwd.to_string_lossy().into_owned()),
        ..Default::default()
    };
    let result = run_command(params, ExecConfig::defaults()).await.unwrap();
    let reported = Path::new(result.stdout.trim());
    let canonical_reported = std::fs::canonicalize(reported).unwrap();
    let canonical_set = std::fs::canonicalize(&cwd).unwrap();
    assert_eq!(canonical_reported, canonical_set);
}

#[tokio::test]
async fn passes_env_overrides() {
    let mut env = HashMap::new();
    env.insert("MY_PROXY_VAR".to_string(), "hello".to_string());
    let params = ExecParams {
        command: "printenv MY_PROXY_VAR".into(),
        env: Some(env),
        ..Default::default()
    };
    let result = run_command(params, ExecConfig::defaults()).await.unwrap();
    assert_eq!(result.stdout, "hello\n");
}
```

- [ ] **Step 2: Run tests to verify the new ones fail**

Run: `cargo test pipes_stdin respects_cwd passes_env_overrides`
Expected: FAIL — `pipes_stdin` gets empty stdout (stdin was `Stdio::null()`); `respects_cwd` reports the proxy's cwd, not temp_dir; `passes_env_overrides` gets empty stdout.

- [ ] **Step 3: Rewrite `run_command` in `src/exec.rs` to honor stdin/cwd/env**

Replace the entire `run_command` function body with:

```rust
use tokio::io::AsyncWriteExt;

pub async fn run_command(params: ExecParams, config: ExecConfig) -> Result<ExecResult, ExecError> {
    let start = Instant::now();
    let timeout = Duration::from_secs(config.default_timeout_secs);

    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(&params.command);
    cmd.kill_on_drop(true);
    if let Some(cwd) = &params.cwd {
        cmd.current_dir(cwd);
    }
    if let Some(env) = &params.env {
        for (k, v) in env {
            cmd.env(k, v);
        }
    }
    cmd.stdin(std::process::Stdio::piped());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let mut child = cmd.spawn().map_err(|e| ExecError::Spawn(e.to_string()))?;

    if let Some(input) = &params.stdin {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(input.as_bytes()).await;
        }
    } else {
        drop(child.stdin.take());
    }

    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(output)) => Ok(ExecResult {
            exit_code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            stdout_truncated: false,
            stderr_truncated: false,
            timed_out: false,
            duration_ms: start.elapsed().as_millis(),
        }),
        Ok(Err(e)) => Err(ExecError::Io(e)),
        Err(_) => Ok(ExecResult {
            exit_code: None,
            stdout: String::new(),
            stderr: String::new(),
            stdout_truncated: false,
            stderr_truncated: false,
            timed_out: true,
            duration_ms: start.elapsed().as_millis(),
        }),
    }
}
```

Add the `use tokio::io::AsyncWriteExt;` import at the top of `src/exec.rs` (with the other `use` statements).

- [ ] **Step 4: Run all tests to verify they pass**

Run: `cargo test`
Expected: all 8 tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/exec.rs tests/exec.rs
git commit -m "feat(exec): honor stdin, cwd, and env overrides"
```

---

## Task 3: exec.rs resource limits — per-call timeout, truncation, process-group kill

**Files:**
- Modify: `src/exec.rs` (rewrite `run_command` to use `params.timeout_secs` with clamping, manual concurrent drain with truncation, `select!`-based timeout, process-group kill; add `drain` + `kill_group` helpers)
- Modify: `tests/exec.rs` (add 3 tests)

**Interfaces:**
- Consumes: `ExecConfig.output_cap_bytes`, `ExecConfig.max_timeout_secs`, `ExecParams.timeout_secs` (declared earlier, now used).
- Produces: no public signature change. `run_command` now: clamps `params.timeout_secs` to `config.max_timeout_secs` (defaulting to `config.default_timeout_secs` when `None`), truncates each output stream to `config.output_cap_bytes`, kills the whole process group on timeout and returns partial output.

- [ ] **Step 1: Write the failing tests (append to `tests/exec.rs`)**

```rust
use std::time::Instant;

#[tokio::test]
async fn timeout_kills_and_reports() {
    let params = ExecParams {
        command: "sleep 10".into(),
        timeout_secs: Some(1),
        ..Default::default()
    };
    let start = Instant::now();
    let result = run_command(params, ExecConfig::defaults()).await.unwrap();
    assert!(result.timed_out);
    assert_eq!(result.exit_code, None);
    assert!(start.elapsed().as_secs() < 5, "should be killed near 1s, not 10s");
}

#[tokio::test]
async fn timeout_clamped_to_max() {
    // timeout_secs (3600) exceeds max_timeout_secs (2) -> clamped to 2.
    let config = ExecConfig {
        max_timeout_secs: 2,
        ..ExecConfig::defaults()
    };
    let params = ExecParams {
        command: "sleep 10".into(),
        timeout_secs: Some(3600),
        ..Default::default()
    };
    let start = Instant::now();
    let result = run_command(params, config).await.unwrap();
    assert!(result.timed_out);
    assert_eq!(result.exit_code, None);
    let elapsed = start.elapsed().as_secs();
    assert!(elapsed >= 2 && elapsed < 8, "should be clamped to ~2s, got {elapsed}s");
}

#[tokio::test]
async fn output_truncation() {
    let config = ExecConfig {
        output_cap_bytes: 16,
        ..ExecConfig::defaults()
    };
    let params = ExecParams {
        command: "seq 1 1000".into(),
        ..Default::default()
    };
    let result = run_command(params, config).await.unwrap();
    assert_eq!(result.exit_code, Some(0));
    assert!(result.stdout_truncated);
    assert_eq!(result.stdout.len(), 16);
}
```

- [ ] **Step 2: Run tests to verify the new ones fail**

Run: `cargo test timeout_kills_and_reports timeout_clamped_to_max output_truncation`
Expected: FAIL — `timeout_kills_and_reports` runs the full default 120s timeout (or 10s) because `params.timeout_secs` is ignored; `output_truncation` returns full output with `stdout_truncated == false`.

- [ ] **Step 3: Rewrite `src/exec.rs` with the full implementation**

Replace the entire contents of `src/exec.rs` with:

```rust
use std::collections::HashMap;
use std::time::{Duration, Instant};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ExecParams {
    pub command: String,
    pub cwd: Option<String>,
    pub env: Option<HashMap<String, String>>,
    pub timeout_secs: Option<u64>,
    pub stdin: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExecResult {
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub timed_out: bool,
    pub duration_ms: u128,
}

#[derive(Debug, Clone, Copy)]
pub struct ExecConfig {
    pub output_cap_bytes: usize,
    pub default_timeout_secs: u64,
    pub max_timeout_secs: u64,
}

impl ExecConfig {
    pub fn defaults() -> Self {
        Self {
            output_cap_bytes: 102_400,
            default_timeout_secs: 120,
            max_timeout_secs: 1800,
        }
    }
}

#[derive(Debug, Error)]
pub enum ExecError {
    #[error("spawn failed: {0}")]
    Spawn(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub async fn run_command(params: ExecParams, config: ExecConfig) -> Result<ExecResult, ExecError> {
    let start = Instant::now();
    let timeout_secs = params
        .timeout_secs
        .unwrap_or(config.default_timeout_secs)
        .min(config.max_timeout_secs);
    let timeout = Duration::from_secs(timeout_secs);

    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(&params.command);
    cmd.process_group(0);
    cmd.kill_on_drop(true);
    if let Some(cwd) = &params.cwd {
        cmd.current_dir(cwd);
    }
    if let Some(env) = &params.env {
        for (k, v) in env {
            cmd.env(k, v);
        }
    }
    cmd.stdin(std::process::Stdio::piped());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let mut child = cmd.spawn().map_err(|e| ExecError::Spawn(e.to_string()))?;

    if let Some(input) = &params.stdin {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(input.as_bytes()).await;
        }
    } else {
        drop(child.stdin.take());
    }

    let cap = config.output_cap_bytes;
    let stdout = child.stdout.take().expect("stdout piped");
    let stderr = child.stderr.take().expect("stderr piped");
    let stdout_task = tokio::spawn(drain(stdout, cap));
    let stderr_task = tokio::spawn(drain(stderr, cap));

    let result = tokio::select! {
        status = child.wait() => {
            let status = status?;
            let (out_buf, out_trunc) = stdout_task.await??;
            let (err_buf, err_trunc) = stderr_task.await??;
            ExecResult {
                exit_code: status.code(),
                stdout: String::from_utf8_lossy(&out_buf).into_owned(),
                stderr: String::from_utf8_lossy(&err_buf).into_owned(),
                stdout_truncated: out_trunc,
                stderr_truncated: err_trunc,
                timed_out: false,
                duration_ms: start.elapsed().as_millis(),
            }
        }
        _ = tokio::time::sleep(timeout) => {
            if let Some(pid) = child.id() {
                kill_group(pid);
            }
            let _ = child.wait().await;
            let (out_buf, out_trunc) = stdout_task.await??;
            let (err_buf, err_trunc) = stderr_task.await??;
            ExecResult {
                exit_code: None,
                stdout: String::from_utf8_lossy(&out_buf).into_owned(),
                stderr: String::from_utf8_lossy(&err_buf).into_owned(),
                stdout_truncated: out_trunc,
                stderr_truncated: err_trunc,
                timed_out: true,
                duration_ms: start.elapsed().as_millis(),
            }
        }
    };
    Ok(result)
}

/// Read `r` to EOF, keeping at most `cap` bytes. Returns `(buf, truncated)`.
/// Continues reading (and discarding) past the cap so a child producing more
/// than `cap` bytes does not block forever on a full pipe.
async fn drain<R: tokio::io::AsyncRead + Unpin>(
    mut r: R,
    cap: usize,
) -> std::io::Result<(Vec<u8>, bool)> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 8192];
    let mut truncated = false;
    loop {
        let n = r.read(&mut tmp).await?;
        if n == 0 {
            break;
        }
        if buf.len() < cap {
            let remaining = cap - buf.len();
            let take = n.min(remaining);
            buf.extend_from_slice(&tmp[..take]);
            if take < n {
                truncated = true;
            }
        } else {
            truncated = true;
        }
    }
    Ok((buf, truncated))
}

/// Send SIGKILL to the entire process group led by `pid`.
/// `process_group(0)` on spawn made the child a group leader with pgid == pid,
/// so `-pid` targets the whole group (e.g. `npm | something`).
fn kill_group(pid: u32) {
    unsafe {
        libc::kill(-(pid as i32), libc::SIGKILL);
    }
}
```

- [ ] **Step 4: Run all tests to verify they pass**

Run: `cargo test`
Expected: all 11 tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/exec.rs tests/exec.rs
git commit -m "feat(exec): per-call timeout with clamping, output truncation, process-group kill"
```

---

## Task 4: config resolution — `ServerConfig::resolve()` + `parse_kv()`

**Files:**
- Modify: `src/config.rs` (replace stub with full implementation + unit tests)

**Interfaces:**
- Consumes: `ExecConfig` (defined in `exec.rs` since Task 1; stays there because `run_command` uses it).
- Produces: `pub struct ServerConfig { pub exec: ExecConfig, pub log_dir: PathBuf }`; `pub fn ServerConfig::resolve() -> Result<ServerConfig, String>`; `pub fn parse_kv(contents: &str) -> Vec<(String, String)>`. `config.rs` re-exports `ExecConfig` via `pub use crate::exec::ExecConfig;` so `server.rs` can import everything from `config`. `ServerConfig::resolve()` reads the config file (if present), then applies env overrides. `server.rs` (Task 5) will call `ServerConfig::resolve()`.

- [ ] **Step 1: Write the failing unit tests at the bottom of `src/config.rs`**

First replace the stub content of `src/config.rs` with the test module only (the implementation comes next). Put this in `src/config.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::parse_kv;

    #[test]
    fn parses_key_value() {
        let result: std::collections::HashMap<String, String> =
            parse_kv("output_cap_bytes = 2048\n").into_iter().collect();
        assert_eq!(result.get("output_cap_bytes").unwrap(), "2048");
    }

    #[test]
    fn handles_spaces_around_eq() {
        let result: std::collections::HashMap<String, String> =
            parse_kv("default_timeout_secs   =    30\n").into_iter().collect();
        assert_eq!(result.get("default_timeout_secs").unwrap(), "30");
    }

    #[test]
    fn ignores_comments_and_blank_lines() {
        let input = "# a comment\n\ndefault_timeout_secs = 5\n  # indented comment\n";
        let result: std::collections::HashMap<String, String> = parse_kv(input).into_iter().collect();
        assert_eq!(result.len(), 1);
        assert_eq!(result.get("default_timeout_secs").unwrap(), "5");
    }

    #[test]
    fn ignores_unknown_keys() {
        let result: std::collections::HashMap<String, String> =
            parse_kv("bogus = 1\nlog_dir = /tmp/x\n").into_iter().collect();
        assert_eq!(result.len(), 2);
        assert!(result.contains_key("bogus"));
        assert_eq!(result.get("log_dir").unwrap(), "/tmp/x");
    }

    #[test]
    fn empty_input_yields_empty() {
        assert!(parse_kv("").is_empty());
        assert!(parse_kv("# only comments\n\n").is_empty());
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib config`
Expected: FAIL — `parse_kv` is not defined (compile error).

- [ ] **Step 3: Implement `config.rs` (replace the whole file)**

```rust
use std::env;
use std::path::PathBuf;

pub use crate::exec::ExecConfig;

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub exec: ExecConfig,
    pub log_dir: PathBuf,
}

impl ServerConfig {
    /// Resolve config: built-in defaults <- config file <- environment variables.
    pub fn resolve() -> Result<Self, String> {
        let mut output_cap_bytes: usize = 102_400;
        let mut default_timeout_secs: u64 = 120;
        let mut max_timeout_secs: u64 = 1800;
        let mut log_dir: PathBuf = default_log_dir();

        let cfg_path = config_file_path();
        if cfg_path.exists() {
            let contents = std::fs::read_to_string(&cfg_path)
                .map_err(|e| format!("read {}: {e}", cfg_path.display()))?;
            for (key, val) in parse_kv(&contents) {
                match key.as_str() {
                    "output_cap_bytes" => {
                        if let Ok(v) = val.parse() {
                            output_cap_bytes = v;
                        }
                    }
                    "default_timeout_secs" => {
                        if let Ok(v) = val.parse() {
                            default_timeout_secs = v;
                        }
                    }
                    "max_timeout_secs" => {
                        if let Ok(v) = val.parse() {
                            max_timeout_secs = v;
                        }
                    }
                    "log_dir" => log_dir = PathBuf::from(val),
                    _ => {}
                }
            }
        }

        if let Some(v) = env_parse("MCP_CLI_PROXY_OUTPUT_CAP") {
            output_cap_bytes = v;
        }
        if let Some(v) = env_parse("MCP_CLI_PROXY_DEFAULT_TIMEOUT") {
            default_timeout_secs = v;
        }
        if let Some(v) = env_parse("MCP_CLI_PROXY_MAX_TIMEOUT") {
            max_timeout_secs = v;
        }
        if let Ok(v) = env::var("MCP_CLI_PROXY_LOG_DIR") {
            if !v.is_empty() {
                log_dir = PathBuf::from(v);
            }
        }

        Ok(Self {
            exec: ExecConfig {
                output_cap_bytes,
                default_timeout_secs,
                max_timeout_secs,
            },
            log_dir,
        })
    }
}

fn env_parse<T: std::str::FromStr>(name: &str) -> Option<T> {
    env::var(name).ok().and_then(|s| s.parse().ok())
}

/// Parse line-based `key = value` config contents. `#` lines are comments.
/// Whitespace around `=` and the value is trimmed. Unknown keys are preserved
/// (the caller decides what to use) so `parse_kv` stays a pure parser.
pub fn parse_kv(contents: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for raw in contents.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            let key = k.trim().to_string();
            let val = v.trim().to_string();
            if !key.is_empty() {
                out.push((key, val));
            }
        }
    }
    out
}

fn config_file_path() -> PathBuf {
    config_dir().join("mcp-cli-proxy").join("config")
}

fn config_dir() -> PathBuf {
    if let Ok(xdg) = env::var("XDG_CONFIG_HOME") {
        if !xdg.is_empty() {
            return PathBuf::from(xdg);
        }
    }
    PathBuf::from(env::var("HOME").unwrap_or_else(|_| ".".into())).join(".config")
}

fn default_log_dir() -> PathBuf {
    config_dir().join("mcp-cli-proxy").join("logs")
}

#[cfg(test)]
mod tests {
    use super::parse_kv;

    #[test]
    fn parses_key_value() {
        let result: std::collections::HashMap<String, String> =
            parse_kv("output_cap_bytes = 2048\n").into_iter().collect();
        assert_eq!(result.get("output_cap_bytes").unwrap(), "2048");
    }

    #[test]
    fn handles_spaces_around_eq() {
        let result: std::collections::HashMap<String, String> =
            parse_kv("default_timeout_secs   =    30\n").into_iter().collect();
        assert_eq!(result.get("default_timeout_secs").unwrap(), "30");
    }

    #[test]
    fn ignores_comments_and_blank_lines() {
        let input = "# a comment\n\ndefault_timeout_secs = 5\n  # indented comment\n";
        let result: std::collections::HashMap<String, String> = parse_kv(input).into_iter().collect();
        assert_eq!(result.len(), 1);
        assert_eq!(result.get("default_timeout_secs").unwrap(), "5");
    }

    #[test]
    fn ignores_unknown_keys() {
        let result: std::collections::HashMap<String, String> =
            parse_kv("bogus = 1\nlog_dir = /tmp/x\n").into_iter().collect();
        assert_eq!(result.len(), 2);
        assert!(result.contains_key("bogus"));
        assert_eq!(result.get("log_dir").unwrap(), "/tmp/x");
    }

    #[test]
    fn empty_input_yields_empty() {
        assert!(parse_kv("").is_empty());
        assert!(parse_kv("# only comments\n\n").is_empty());
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib config`
Expected: 5 config tests pass.

- [ ] **Step 5: Run the full test suite + build to confirm no regressions**

Run: `cargo test && cargo build`
Expected: all tests pass; build succeeds.

- [ ] **Step 6: Commit**

```bash
git add src/config.rs
git commit -m "feat(config): ServerConfig::resolve() with file + env overrides"
```

---

## Task 5: server.rs — rmcp `ServerHandler`, `exec_command` tool, dispatch, validation

**Files:**
- Modify: `src/server.rs` (replace stub with full `ServerHandler` impl)
- No new tests (per spec: no rmcp-level automated tests; verification is a manual smoke test).

**Interfaces:**
- Consumes: `crate::config::ServerConfig` (Task 4), `crate::exec::{run_command, ExecParams, ExecConfig, ExecError}` (Tasks 1–3).
- Produces: `pub async fn run_server() -> Result<(), Box<dyn std::error::Error>>` — the real server (was a stub since Task 1). `cli::run` already calls it.

- [ ] **Step 1: Replace `src/server.rs` with the full implementation**

```rust
use rmcp::{
    model::{
        CallToolRequestParams, CallToolResponse, ContentBlock, ErrorData, Implementation,
        ListToolsResult, PaginatedRequestParams, ProtocolVersion, ServerCapabilities, ServerInfo,
        Tool,
    },
    service::{MaybeSendFuture, RequestContext},
    RoleServer, ServerHandler, ServiceExt,
};
use std::collections::HashMap;
use std::future::Future;
use std::path::Path;
use std::sync::Arc;

use crate::config::ServerConfig;
use crate::exec::{run_command, ExecConfig, ExecParams};

pub async fn run_server() -> Result<(), Box<dyn std::error::Error>> {
    let server_cfg = ServerConfig::resolve()?;
    std::fs::create_dir_all(&server_cfg.log_dir)?;

    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(server_cfg.log_dir.join("server.log"))?;
    let _ = tracing_subscriber::fmt()
        .with_writer(std::sync::Mutex::new(log_file))
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();
    tracing::info!(
        "mcp-cli-proxy starting: log_dir={}, output_cap_bytes={}, default_timeout_secs={}, max_timeout_secs={}",
        server_cfg.log_dir.display(),
        server_cfg.exec.output_cap_bytes,
        server_cfg.exec.default_timeout_secs,
        server_cfg.exec.max_timeout_secs
    );

    let server = ProxyServer {
        config: Arc::new(server_cfg.exec),
    };
    let (stdin, stdout) = rmcp::transport::io::stdio();
    let running = server.serve((stdin, stdout)).await?;
    running.waiting().await?;
    tracing::info!("mcp-cli-proxy shutting down (stdin closed)");
    Ok(())
}

struct ProxyServer {
    config: Arc<ExecConfig>,
}

fn exec_tool_schema() -> serde_json::Map<String, serde_json::Value> {
    serde_json::from_str(
        r#"{
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "Shell command to run on the host (outside the sandbox). Uses sh -c, so pipes, globs, and redirects work." },
                "cwd": { "type": "string", "description": "Working directory. Defaults to the proxy's cwd." },
                "env": { "type": "object", "additionalProperties": { "type": "string" }, "description": "Extra env vars, merged over the inherited environment." },
                "timeout_secs": { "type": "integer", "minimum": 1, "description": "Per-call timeout in seconds. Clamped to max_timeout_secs." },
                "stdin": { "type": "string", "description": "Bytes piped to the command's stdin." }
            },
            "required": ["command"]
        }"#,
    )
    .unwrap()
}

const EXEC_TOOL_DESCRIPTION: &str = "Execute an arbitrary shell command on the host machine — outside the sandbox. Use this when a command is blocked by the sandboxed environment, or when you need host-level network/filesystem access (git over network, curl, builds, pod install, etc.). Returns stdout, stderr, exit_code. Supports pipes, globs, and redirects via sh -c.";

impl ServerHandler for ProxyServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_protocol_version(ProtocolVersion::V_2024_11_05)
            .with_server_info(Implementation::new("mcp-cli-proxy", env!("CARGO_PKG_VERSION")))
    }

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, ErrorData>> + MaybeSendFuture + '_ {
        std::future::ready(Ok(ListToolsResult::with_all_items(vec![Tool::new(
            "exec_command",
            EXEC_TOOL_DESCRIPTION,
            exec_tool_schema(),
        )])))
    }

    fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CallToolResponse, ErrorData>> + MaybeSendFuture + '_ {
        let args = request.arguments.clone().unwrap_or_default();
        let config = self.config.clone();
        async move { dispatch(&args, &config).await }
    }
}

async fn dispatch(
    args: &serde_json::Map<String, serde_json::Value>,
    config: &ExecConfig,
) -> Result<CallToolResponse, ErrorData> {
    // Validate command (tool-level error -> isError result, not protocol error).
    let command = match args.get("command").and_then(|v| v.as_str()) {
        Some(s) if !s.trim().is_empty() => s.trim().to_string(),
        _ => return Ok(tool_error("command is required")),
    };

    // Validate cwd.
    let cwd = args.get("cwd").and_then(|v| v.as_str()).map(|s| s.to_string());
    if let Some(c) = &cwd {
        let p = Path::new(c);
        if !p.exists() {
            return Ok(tool_error(&format!("cwd does not exist: {c}")));
        }
        if !p.is_dir() {
            return Ok(tool_error(&format!("cwd is not a directory: {c}")));
        }
    }

    // Validate timeout_secs.
    let timeout_secs = args.get("timeout_secs").and_then(|v| v.as_u64());
    if let Some(t) = timeout_secs {
        if t == 0 {
            return Ok(tool_error("timeout_secs must be greater than 0"));
        }
    }

    // Parse env (string -> string map).
    let env: Option<HashMap<String, String>> = args
        .get("env")
        .and_then(|v| v.as_object())
        .map(|obj| {
            obj.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        });

    let stdin = args.get("stdin").and_then(|v| v.as_str()).map(|s| s.to_string());

    let params = ExecParams {
        command,
        cwd,
        env,
        timeout_secs,
        stdin,
    };

    match run_command(params, *config).await {
        Ok(result) => {
            let text = serde_json::to_string_pretty(&result)
                .unwrap_or_else(|_| result.to_string());
            Ok(CallToolResponse::Complete(
                rmcp::model::CallToolResult::success(vec![ContentBlock::text(text)]),
            ))
        }
        Err(e) => Err(ErrorData::internal_error(
            format!("exec failed: {e}"),
            None,
        )),
    }
}

/// Build a tool-level error result (sets `isError: true` on the MCP response).
fn tool_error(msg: &str) -> CallToolResponse {
    CallToolResponse::Complete(rmcp::model::CallToolResult::error(vec![
        ContentBlock::text(msg.to_string()),
    ]))
}
```

- [ ] **Step 2: Build to verify it compiles**

Run: `cargo build`
Expected: compiles cleanly. If `ErrorData::internal_error` is not found in this rmcp version, replace with `ErrorData::new(rmcp::model::ErrorCode::INTERNAL_ERROR, format!("exec failed: {e}"), None)`.

- [ ] **Step 3: Run the manual smoke test**

Run this end-to-end stdio sequence against the freshly built server:

```bash
(printf '%s\n' \
'{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"smoke","version":"0"}}}' \
'{"jsonrpc":"2.0","method":"notifications/initialized"}' \
'{"jsonrpc":"2.0","id":2,"method":"tools/list"}' \
'{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"exec_command","arguments":{"command":"echo smoke"}}}'; sleep 1) | cargo run -q
```

Expected: the `id:2` response contains an `exec_command` tool; the `id:3` response contains a JSON result with `"exit_code":0` and `"stdout":"smoke\n"`.

- [ ] **Step 4: Verify a tool-level error returns `isError: true`**

Run:

```bash
(printf '%s\n' \
'{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"smoke","version":"0"}}}' \
'{"jsonrpc":"2.0","method":"notifications/initialized"}' \
'{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"exec_command","arguments":{"command":"   "}}}'; sleep 1) | cargo run -q
```

Expected: the `id:2` response has `"isError":true` and content `"command is required"`.

- [ ] **Step 5: Commit**

```bash
git add src/server.rs
git commit -m "feat(server): rmcp ServerHandler with exec_command tool + validation"
```

---

## Task 6: README + AGENTS.md routing snippet

**Files:**
- Create: `README.md`
- Create: `AGENTS.md`

**Interfaces:**
- Consumes: nothing (documentation).
- Produces: user-facing docs: install/run instructions, config reference, host registration, smoke test, and the drop-in agent routing snippet.

- [ ] **Step 1: Create `README.md`**

````markdown
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
````

- [ ] **Step 2: Create `AGENTS.md` (drop-in routing snippet)**

```markdown
# AGENTS.md

## Host commands via proxy

This environment's bash is sandboxed (blocks some network + OS calls).
For git operations that need network, `curl`, builds, `pod install`, or
anything that fails with a sandbox/permission error, call the `exec_command`
tool from the `mcp-cli-proxy` server instead of bash.

`exec_command` runs the command on the host PC (outside the sandbox) via
`sh -c`, so pipes, globs, and redirects work. It returns stdout, stderr, and
exit_code as JSON. Prefer it whenever a normal bash command is blocked.
```

- [ ] **Step 3: Commit**

```bash
git add README.md AGENTS.md
git commit -m "docs: add README and AGENTS.md routing snippet"
```

---

## Self-Review (completed by plan author)

**1. Spec coverage:**
- §1 Architecture (stdio, host subprocess, exec/server split) → Task 1 (scaffold + split), Task 5 (server stdio serve).
- §2 `exec_command` API (input/output schema, tool description) → Task 5 (schema + description + dispatch). ✓
- §3 Project structure (lib + main + cli/exec/config/server + tests/exec) → Task 1. ✓
- §3 Dependencies (exact versions) → Task 1 Cargo.toml; `libc` added in Task 1 (used in Task 3). ✓
- §4 Configuration (resolution order, keys, env vars, logging) → Task 4. ✓
- §4 CLI (optional `serve` default) → Task 1 cli.rs. ✓
- §5 Error layering (tool errors → `isError`; internal → `ErrorData`) → Task 5 dispatch. ✓
- §5 Edge cases (empty command, bad cwd, timeout clamp, timeout kill, truncation, exit 127) → Task 5 validation + Task 3 limits; exit 127 covered by Task 1 test. ✓
- §5 Process-group kill (`libc::kill(-pid)`) → Task 3 `kill_group`. ✓
- §6 Testing (11 exec tests + config tests + smoke test) → Tasks 1–4 + Task 5 smoke. ✓
- §7 Routing & discovery (AGENTS.md snippet) → Task 6. ✓

**2. Placeholder scan:** No TBD/TODO/"add error handling"/"similar to Task N". Every code step contains complete code.

**3. Type consistency:** `ExecParams`, `ExecResult`, `ExecError`, `ExecConfig` (with `defaults()`), `run_command(params, config) -> Result<ExecResult, ExecError>` — consistent across Tasks 1–3 and consumed by Task 5. `ServerConfig::resolve()` and `parse_kv()` consistent between Task 4 definition and Task 5 use. `tool_error` / `dispatch` / `run_server` consistent within Task 5.

No gaps or contradictions found.
