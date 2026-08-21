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

/// Install the tracing subscriber writing to `log_file` (or a sink if None).
/// `try_init` ignores the "already initialized" error so callers in both the
/// bridge and daemon modes can invoke this without coordinating.
pub(crate) fn init_logger(log_file: Option<std::fs::File>) {
    let writer: std::sync::Mutex<Box<dyn std::io::Write + Send>> = match log_file {
        Some(file) => std::sync::Mutex::new(Box::new(file)),
        None => std::sync::Mutex::new(Box::new(std::io::sink())),
    };
    let _ = tracing_subscriber::fmt()
        .with_writer(writer)
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();
}
