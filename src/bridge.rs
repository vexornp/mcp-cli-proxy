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

#[derive(Debug)]
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

#[cfg(test)]
impl RemoteExecutor {
    fn from_stream(stream: UnixStream) -> Self {
        Self {
            stream: Mutex::new(stream),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn remote_executor_round_trips_one_call() {
        let (server_sock, client_sock) = UnixStream::pair().unwrap();

        let server = tokio::spawn(async move {
            let mut conn = server_sock;
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

        let exec = RemoteExecutor::from_stream(client_sock);
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
