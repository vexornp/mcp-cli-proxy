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

    // Validate timeout_secs (must be > 0 if provided; negatives caught via as_i64).
    let timeout_secs = match args.get("timeout_secs") {
        None => None,
        Some(v) => match v.as_i64() {
            Some(t) if t > 0 => Some(t as u64),
            _ => return Ok(tool_error("timeout_secs must be greater than 0")),
        },
    };

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
                .unwrap_or_else(|_| serde_json::to_string(&result).unwrap_or_default());
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
