use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;

use tokio::net::{UnixListener, UnixStream};
use tokio::signal::unix::{signal, SignalKind};

use crate::bridge::DaemonOptions;
use crate::config::ServerConfig;
use crate::exec::{run_command, ExecConfig, ExecParams, ExecResult};
use crate::framing::{read_frame, write_frame};
use crate::log::{init_logger, resolve_log};

pub async fn run_daemon(opts: DaemonOptions) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let log_dir_cfg = ServerConfig::resolve()
        .map(|c| c.log_dir)
        .unwrap_or_else(|_| std::env::temp_dir().join("mcp-cli-proxy").join("logs"));
    let (log_dir, log_file) = resolve_log(&log_dir_cfg);
    init_logger(log_file);
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
    // TOCTOU: a peer could swap the socket file between bind and set_permissions.
    // Accepted on a single-user Mac; revisit if shared with other users.
    std::fs::set_permissions(
        &opts.socket_path,
        std::fs::Permissions::from_mode(0o600),
    )?;
    tracing::info!("daemon listening on {}", opts.socket_path.display());

    let config = Arc::new(opts.config);
    let socket_path = opts.socket_path.clone();

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

#[cfg(test)]
mod tests {
    use super::*;

    // handle_conn is testable in-sandbox via UnixStream::pair() (socketpair),
    // which the sandbox permits — unlike UnixListener::bind (sandbox-blocked).
    // The full daemon integration tests in tests/daemon_bridge.rs are #[ignore]'d
    // and run unsandboxed via: cargo test --test daemon_bridge -- --ignored
    #[tokio::test]
    async fn handle_conn_round_trips_one_command() {
        let config = ExecConfig::defaults();
        let (mut client, server) = UnixStream::pair().unwrap();

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
        let (mut client, server) = UnixStream::pair().unwrap();

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
