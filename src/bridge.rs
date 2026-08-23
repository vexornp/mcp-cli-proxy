use std::future::Future;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::pin::Pin;

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
