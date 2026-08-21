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
