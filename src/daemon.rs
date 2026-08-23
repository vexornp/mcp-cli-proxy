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
    let log_path = log_dir.join("server.log");
    let cfg = &opts.config;
    eprintln!(
        "mcp-cli-proxy daemon\n  \
         listening:  {addr}\n  \
         log file:   {log}\n  \
         pid:        {pid}\n  \
         exec limits (enforced here):\n    \
           output cap:       {cap} bytes ({cap_kb} KB)\n    \
           default timeout:  {def}s\n    \
           max timeout:      {max}s\n  \
         stop:       Ctrl-C or SIGTERM",
        addr = bound_addr,
        log = log_path.display(),
        pid = std::process::id(),
        cap = cfg.output_cap_bytes,
        cap_kb = cfg.output_cap_bytes / 1024,
        def = cfg.default_timeout_secs,
        max = cfg.max_timeout_secs,
    );
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
