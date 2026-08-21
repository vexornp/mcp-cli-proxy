use std::collections::HashMap;
use std::time::{Duration, Instant};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::io::AsyncWriteExt;
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
