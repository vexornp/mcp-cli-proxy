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

impl From<tokio::task::JoinError> for ExecError {
    fn from(e: tokio::task::JoinError) -> Self {
        ExecError::Io(std::io::Error::other(e))
    }
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
                exit_code: exit_code_from_status(&status),
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

/// Extract exit code from an ExitStatus, following the Unix convention
/// (128 + signal number) for signal-terminated processes.
fn exit_code_from_status(status: &std::process::ExitStatus) -> Option<i32> {
    status.code().or_else(|| {
        use std::os::unix::process::ExitStatusExt;
        status.signal().map(|s| 128 + s)
    })
}
