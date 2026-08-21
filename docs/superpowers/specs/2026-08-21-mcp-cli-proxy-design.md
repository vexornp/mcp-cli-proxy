# mcp-cli-proxy — Design

- **Date:** 2026-08-21
- **Status:** Approved (pending spec review)
- **Project:** `mcp-cli-proxy`
- **Stack:** Rust + `rmcp` (mirrors the `XcodeMcp` project conventions)

## Background & motivation

The user's AI agent runs in a **sandboxed environment** where some CLI calls and
network requests are blocked. The agent still needs to perform those operations
(e.g. `git` over network, `curl`, builds, `pod install`).

The agent host (logoscode / Claude Desktop / similar) launches MCP servers as
**local subprocesses on the user's PC**. Because the *host* runs on the PC, those
subprocesses run outside the agent's bash/network sandbox. `mcp-cli-proxy`
exploits this: it is an MCP server that exposes a generic `exec_command` tool.
When the agent calls it, the proxy runs the command locally on the PC (not
sandboxed) and returns the result.

## Goals

- Provide a single generic `exec_command` MCP tool that runs arbitrary shell
  commands on the host PC, bypassing the agent's sandbox.
- Communicate over **stdio** (local subprocess), matching the user's existing
  `xcode-mcp` setup and toolchain.
- Keep the execution logic fully decoupled from MCP so it is unit-testable.
- Follow the conventions established by the `XcodeMcp` project (Rust, `rmcp`,
  clap CLI, tracing logs, env-var + config-file resolution).

## Non-goals (YAGNI — parked for later)

- Network transport (HTTP/SSE/TCP). The core is transport-agnostic enough to add
  later, but stdio is sufficient now.
- Translucent forwarding of a specific real MCP server's tools. The proxy is a
  generic executor the agent calls explicitly.
- A no-shell `args[]` execution mode. Shell-string is sufficient.
- A `debug` CLI subcommand. Add when needed.
- Output spilling to files. Inline truncation is sufficient.
- Automatic interception/rerouting of failed sandboxed bash calls. That is
  agent-**host** behavior, out of scope for the proxy.

## Decisions summary

| Decision | Choice | Rationale |
|---|---|---|
| Role | Generic executor | Simplest, composable; agent calls proxy's tools explicitly. |
| Transport | stdio (local subprocess) | Matches `xcode-mcp`; host runs on PC so subprocess is unsandboxed. |
| Tool surface | Single `exec_command` | YAGNI; one tool covers CLI + network (via `curl`). |
| Security | Unrestricted + per-call timeout | Personal dev box; maximum power. Timeout is robustness, not a gate. |
| Result handling | Inline stdout/stderr/exit_code, truncate at per-stream cap | Simple; protects agent context window. |
| Project structure | Single crate + decoupled `exec.rs` module + `tests/` | Testable without workspace boilerplate (justified for one tool). |
| Execution model | Shell string via `sh -c` | Agent-ergonomic; supports pipes/globs/redirects natively. |

## Section 1 — Architecture & data flow

```
┌───────────────────────────┐        ┌──────────────────────────────────┐
│  Agent (sandboxed bash,   │        │  mcp-cli-proxy  (your PC, NOT    │
│  blocked network)         │        │  sandboxed)                      │
│                           │  stdio │                                  │
│  host (logoscode/etc.) ───┼────────┼─▶ rmcp ServerHandler             │
│  launches proxy as a      │ (JSON- │      │                            │
│  local subprocess on PC   │  RPC)  │      ▼                            │
│                           │        │  exec::run_command()              │
│  calls exec_command tool  │────────┼─▶ tokio::process::Command::      │
│                           │        │      new("sh").arg("-c", cmd)     │
│                           │        │      │                            │
│                           │        │      ▼ runs on your PC            │
│  ◀── result JSON ─────────┼────────┼─  capture stdout/stderr/exit     │
└───────────────────────────┘        └──────────────────────────────────┘
```

- The agent host launches `mcp-cli-proxy` as a local subprocess on the user's
  PC, exactly like `xcode-mcp`. The subprocess runs outside the agent sandbox.
- The agent calls the `exec_command` tool. `ServerHandler::call_tool` dispatches
  to `exec::run_command()`, which spawns the command via `tokio::process::Command`
  with a timeout, captures output, truncates, and returns a JSON result wrapped
  in a `ContentBlock::text` (same pattern as `xcode-mcp`'s `dispatch_tool`).
- `exec.rs` has **zero** `rmcp` dependencies — it takes a plain `ExecParams`
  struct and returns `Result<ExecResult, ExecError>`. `ExecError` (via
  `thiserror`) covers only true internal failures (spawn failure, I/O
  catastrophe); `server.rs` maps `ExecError` → `ErrorData`. Tool-level input
  validation (empty command, bad cwd/timeout) is done in `server.rs` *before*
  calling `run_command` and returned as MCP tool errors. This decoupling is what
  makes `exec.rs` unit-testable in isolation.

## Section 2 — `exec_command` tool API

### Input schema (JSON the agent sends)

| Field | Type | Required | Default | Notes |
|---|---|---|---|---|
| `command` | string | yes | — | Shell command string, run via `sh -c` (Unix). Supports pipes/globs/redirection naturally. |
| `cwd` | string | no | proxy's cwd | Working directory. Unrestricted (any path). |
| `env` | object<string,string> | no | — | Extra/override env vars, merged over the inherited environment. |
| `timeout_secs` | integer | no | `default_timeout_secs` (120) | Per-call timeout. Clamped to `max_timeout_secs` (1800). |
| `stdin` | string | no | — | Bytes piped to the command's stdin. |

JSON Schema (advertised via `tools/list`):

```json
{
  "type": "object",
  "properties": {
    "command": { "type": "string", "description": "Shell command to run on the host (outside the sandbox). Uses sh -c, so pipes, globs, and redirects work." },
    "cwd": { "type": "string", "description": "Working directory. Defaults to the proxy's cwd." },
    "env": { "type": "object", "additionalProperties": { "type": "string" }, "description": "Extra env vars, merged over the inherited environment." },
    "timeout_secs": { "type": "integer", "minimum": 1, "description": "Per-call timeout in seconds. Clamped to max_timeout_secs." },
    "stdin": { "type": "string", "description": "Bytes piped to the command's stdin." }
  },
  "required": ["command"]
}
```

### Output (JSON returned to the agent)

| Field | Type | Notes |
|---|---|---|
| `exit_code` | int \| null | `null` **only** when `timed_out` is true (process killed before exit). Always an integer otherwise, including non-zero exits and exit 127 (command not found). Spawn failure does not appear here — it is an `ExecError` surfaced as `ErrorData`. |
| `stdout` | string | Captured stdout, truncated to `output_cap_bytes`. |
| `stderr` | string | Captured stderr, truncated to `output_cap_bytes` (independent cap per stream). |
| `stdout_truncated` | bool | True if stdout was cut. |
| `stderr_truncated` | bool | True if stderr was cut. |
| `timed_out` | bool | True if the timeout fired and we killed the process. |
| `duration_ms` | int | Wall-clock runtime. |

The result is serialized to pretty JSON and returned as a single
`ContentBlock::text` (matching `xcode-mcp`).

### Tool description (advertised to the agent)

> Execute an arbitrary shell command on the host machine — outside the sandbox.
> Use this when a command is blocked by the sandboxed environment, or when you
> need host-level network/filesystem access (git over network, curl, builds, pod
> install, etc.). Returns stdout, stderr, exit_code. Supports pipes, globs, and
> redirects via `sh -c`.

## Section 3 — Project structure & modules

```
mcp-cli-proxy/
├── Cargo.toml
├── README.md
├── .gitignore
├── docs/
│   └── superpowers/specs/2026-08-21-mcp-cli-proxy-design.md
└── src/
    ├── main.rs        # clap entry; dispatches to server::run_server()
    ├── cli.rs         # Cli struct (serve subcommand, default)
    ├── server.rs      # rmcp ServerHandler impl, tool list, dispatch -> exec
    └── exec.rs        # ExecParams, ExecResult, run_command() — no rmcp deps
└── tests/
    └── exec.rs        # integration tests exercising run_command() directly
```

Module responsibilities:

- **`exec.rs`** — the testable core. Defines `ExecParams` / `ExecResult` (plain
  serde structs), `ExecError` (thiserror enum for spawn/IO failures), and
  `pub async fn run_command(params, config) -> Result<ExecResult, ExecError>`.
  Spawns `sh -c`, pipes stdin, captures stdout/stderr concurrently, enforces
  timeout via `tokio::time::timeout`, truncates each stream. **No `rmcp` import.**
- **`server.rs`** — the rmcp shell. Builds the single `Tool` (`exec_command` +
  its JSON schema), implements `ServerHandler`, resolves config, sets up tracing,
  and in `call_tool` deserializes args → `ExecParams` → calls
  `exec::run_command` → serializes `ExecResult` to pretty JSON → wraps in
  `ContentBlock::text`. Mirrors `xcode-mcp`'s `server.rs` shape (config
  resolution, `serve((stdin, stdout))`, etc.).
- **`cli.rs`** — clap `Cli` with optional `serve` subcommand (default), like
  `xcode-mcp`.
- **`main.rs`** — `#[tokio::main]`, parses CLI, calls `server::run_server()`.

### Dependencies (`Cargo.toml`)

Mirrors `xcode-mcp`:

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
```

`tests/exec.rs` uses only the above dependencies (tokio is already a main dep),
so no separate `[dev-dependencies]` are required.

## Section 4 — Configuration & CLI

### Config resolution

Same pattern as `xcode-mcp`'s `resolve_root_path`:

1. Environment variables (per-process override).
2. Config file at `~/.config/mcp-cli-proxy/config` (XDG-aware via
   `XDG_CONFIG_HOME`), simple `key = value` lines, `#` comments. Reuse the
   line-based parser style from `xcode-mcp` (`parse_config_root`).

### Config keys (all optional, with sensible defaults)

| Key | Env var | Default | Notes |
|---|---|---|---|
| `output_cap_bytes` | `MCP_CLI_PROXY_OUTPUT_CAP` | 102400 (100KB) | Per-stream truncation cap. |
| `default_timeout_secs` | `MCP_CLI_PROXY_DEFAULT_TIMEOUT` | 120 | Used when `timeout_secs` arg omitted. |
| `max_timeout_secs` | `MCP_CLI_PROXY_MAX_TIMEOUT` | 1800 | Hard ceiling on `timeout_secs` arg. |
| `log_dir` | `MCP_CLI_PROXY_LOG_DIR` | `~/.config/mcp-cli-proxy/logs` | tracing log file location. |

No `root`/allowlist keys — the proxy is unrestricted by design.

### CLI (clap)

```
mcp-cli-proxy [serve]      # default: run the stdio MCP server
```

`serve` is optional and the default (matches `xcode-mcp` ergonomics). No `debug`
subcommand initially.

### Logging

`tracing_subscriber::fmt` to `<log_dir>/server.log` (append), env-filter via
`RUST_LOG`. Same as `xcode-mcp`.

## Section 5 — Error handling & edge cases

Two layers, mirroring `xcode-mcp`:

1. **Tool-level errors** (bad input from agent) → return an MCP *error result*
   (`CallToolResult::error` with a text message). The agent sees `isError: true`
   + the message. Examples: `command` missing/empty, `timeout_secs` ≤ 0, `cwd`
   doesn't exist / isn't a directory.
2. **Internal failures** (spawn failed, I/O catastrophe) → `ErrorData` from
   `call_tool` (e.g. `sh` binary not found — extremely unlikely but handled).

### Edge cases & behavior

| Case | Behavior |
|---|---|
| `command` empty/whitespace | Tool error: "command is required". |
| `cwd` set but missing | Tool error: "cwd does not exist: …". |
| `cwd` set but not a directory | Tool error. |
| `timeout_secs` > `max_timeout_secs` | Clamp to `max_timeout_secs` (don't error — friendlier). |
| `timeout_secs` ≤ 0 | Tool error. |
| Timeout fires | Kill the process group, set `timed_out=true`, `exit_code=null`, return partial stdout/stderr captured so far. |
| Output exceeds cap | Keep first `output_cap_bytes` of each stream, set `*_truncated=true`. |
| Command exits non-zero | Normal result: `exit_code=<n>` + stderr. Not a tool error — the command "ran", it just failed. |
| Binary not found (e.g. `foo`) | `sh -c` returns exit 127; surfaced as `exit_code=127` + stderr. Not a tool error. |
| Huge `stdin` | Accept as-is (no input cap; YAGNI). |
| Malformed JSON args | `ErrorData::invalid_params`. |

### Process group killing

Spawn with `process_group(0)` so a timeout kills the whole process tree
(e.g. `npm | something`), not just `sh`. Use `kill_on_drop(true)` so dropping
the handle reaps the child. On timeout, kill the group and await cleanup before
returning partial output.

## Section 6 — Testing

Integration tests in `tests/exec.rs` (the `exec.rs` module has no `rmcp` deps,
so tests are pure):

| Test | What it asserts |
|---|---|
| `runs_simple_command` | `echo hello` → exit 0, stdout "hello\n". |
| `captures_stderr_separately` | `sh -c 'echo out; echo err >&2'` → stdout/stderr split correctly. |
| `pipes_stdin` | `cat` with stdin="hi" → stdout "hi". |
| `respects_cwd` | `pwd` with cwd=/tmp → stdout contains /tmp. |
| `passes_env_overrides` | `printenv FOO` with env FOO=bar → stdout "bar". |
| `nonzero_exit` | `false` → exit_code 1. |
| `command_not_found_exit_code` | `this_does_not_exist_xyz` → exit_code 127. |
| `timeout_kills_and_reports` | `sleep 10` with timeout_secs=1 → timed_out=true, exit_code=null, duration ~1s. |
| `output_truncation` | `seq`/`yes` with a tiny cap → truncated=true, len == cap. |
| `pipes_and_globs_work` | `echo a; echo b | grep a` (via `sh -c`) → confirms shell semantics. |

Config-clamping test: `timeout_secs` over the max is clamped, not rejected.

No rmcp-level tests — we do not spin up a full MCP client. The `server.rs`
dispatch is thin (deserialize → call `run_command` → serialize); the
`tests/exec.rs` suite covers the real logic. This matches `xcode-mcp`'s approach
(logic in core, tests against core).

### Manual smoke test (documented in README)

A canned `initialize` → `tools/list` → `tools/call` JSON-RPC sequence piped into
`mcp-cli-proxy` over stdio, to verify the server end-to-end.

## Section 7 — Routing & discovery

The proxy is *just another MCP* the agent has access to. There is no automatic
wire-level routing — **the agent decides**, same as it picks any tool. The
"knowledge" of when to use the proxy lives in three places:

1. **The `exec_command` tool description (primary signal).** This is what the
   LLM reads to choose tools. We make it rich and explicit (see Section 2).
2. **Your instructions to the agent (system prompt / `AGENTS.md`).** You tell
   the agent the policy. Drop-in snippet for `AGENTS.md`:

   ```markdown
   ## Host commands via proxy
   This environment's bash is sandboxed (blocks some network + OS calls).
   For git operations that need network, `curl`, builds, or anything that
   fails with a sandbox/permission error, call the `exec_command` tool
   from the `mcp-cli-proxy` server instead of bash.
   ```

3. **Error-driven fallback.** When the sandboxed bash returns a
   permission/sandbox error, the agent retries via `exec_command`. The tool
   description + `AGENTS.md` make this reflexive.

### Operational guidance

Register `mcp-cli-proxy` as one of the agent's MCPs in the host config, and
**don't** also register sandboxed CLI MCPs alongside it — let the proxy be the
single host-side path. E.g. instead of a sandboxed `xcode-mcp`, the agent runs
`xcodebuild` / `pod` through `exec_command`.

### Out of scope

Automatic interception/rerouting of failed bash calls. That is agent-**host**
behavior (logoscode / Claude Desktop level), not something the proxy MCP can
control. The proxy just offers the tool; routing is the agent's job, guided by
the three signals above.

## Future considerations (parked, not in scope)

- Network transport (HTTP/SSE) for remote-agent topologies.
- No-shell `args[]` execution mode.
- Output spilling to files for very large outputs.
- Optional command allowlist / cwd root restriction (if trust model changes).
- A `debug` CLI subcommand.
