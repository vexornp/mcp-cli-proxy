use std::env;
use std::path::PathBuf;

pub use crate::exec::ExecConfig;

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub exec: ExecConfig,
    pub log_dir: PathBuf,
}

impl ServerConfig {
    /// Resolve config: built-in defaults <- config file <- environment variables.
    pub fn resolve() -> Result<Self, String> {
        let mut output_cap_bytes: usize = 102_400;
        let mut default_timeout_secs: u64 = 120;
        let mut max_timeout_secs: u64 = 1800;
        let mut log_dir: PathBuf = default_log_dir();

        let cfg_path = config_file_path();
        if cfg_path.exists() {
            let contents = std::fs::read_to_string(&cfg_path)
                .map_err(|e| format!("read {}: {e}", cfg_path.display()))?;
            for (key, val) in parse_kv(&contents) {
                match key.as_str() {
                    "output_cap_bytes" => {
                        if let Ok(v) = val.parse() {
                            output_cap_bytes = v;
                        }
                    }
                    "default_timeout_secs" => {
                        if let Ok(v) = val.parse() {
                            default_timeout_secs = v;
                        }
                    }
                    "max_timeout_secs" => {
                        if let Ok(v) = val.parse() {
                            max_timeout_secs = v;
                        }
                    }
                    "log_dir" => log_dir = PathBuf::from(val),
                    _ => {}
                }
            }
        }

        if let Some(v) = env_parse("MCP_CLI_PROXY_OUTPUT_CAP") {
            output_cap_bytes = v;
        }
        if let Some(v) = env_parse("MCP_CLI_PROXY_DEFAULT_TIMEOUT") {
            default_timeout_secs = v;
        }
        if let Some(v) = env_parse("MCP_CLI_PROXY_MAX_TIMEOUT") {
            max_timeout_secs = v;
        }
        if let Ok(v) = env::var("MCP_CLI_PROXY_LOG_DIR") {
            if !v.is_empty() {
                log_dir = PathBuf::from(v);
            }
        }

        Ok(Self {
            exec: ExecConfig {
                output_cap_bytes,
                default_timeout_secs,
                max_timeout_secs,
            },
            log_dir,
        })
    }
}

fn env_parse<T: std::str::FromStr>(name: &str) -> Option<T> {
    env::var(name).ok().and_then(|s| s.parse().ok())
}

/// Parse line-based `key = value` config contents. `#` lines are comments.
/// Whitespace around `=` and the value is trimmed. Unknown keys are preserved
/// (the caller decides what to use) so `parse_kv` stays a pure parser.
pub fn parse_kv(contents: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for raw in contents.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            let key = k.trim().to_string();
            let val = v.trim().to_string();
            if !key.is_empty() {
                out.push((key, val));
            }
        }
    }
    out
}

fn config_file_path() -> PathBuf {
    config_dir().join("mcp-cli-proxy").join("config")
}

fn config_dir() -> PathBuf {
    if let Ok(xdg) = env::var("XDG_CONFIG_HOME") {
        if !xdg.is_empty() {
            return PathBuf::from(xdg);
        }
    }
    PathBuf::from(env::var("HOME").unwrap_or_else(|_| ".".into())).join(".config")
}

fn default_log_dir() -> PathBuf {
    config_dir().join("mcp-cli-proxy").join("logs")
}

#[cfg(test)]
mod tests {
    use super::parse_kv;

    #[test]
    fn parses_key_value() {
        let result: std::collections::HashMap<String, String> =
            parse_kv("output_cap_bytes = 2048\n").into_iter().collect();
        assert_eq!(result.get("output_cap_bytes").unwrap(), "2048");
    }

    #[test]
    fn handles_spaces_around_eq() {
        let result: std::collections::HashMap<String, String> =
            parse_kv("default_timeout_secs   =    30\n")
                .into_iter()
                .collect();
        assert_eq!(result.get("default_timeout_secs").unwrap(), "30");
    }

    #[test]
    fn ignores_comments_and_blank_lines() {
        let input = "# a comment\n\ndefault_timeout_secs = 5\n  # indented comment\n";
        let result: std::collections::HashMap<String, String> =
            parse_kv(input).into_iter().collect();
        assert_eq!(result.len(), 1);
        assert_eq!(result.get("default_timeout_secs").unwrap(), "5");
    }

    #[test]
    fn ignores_unknown_keys() {
        let result: std::collections::HashMap<String, String> =
            parse_kv("bogus = 1\nlog_dir = /tmp/x\n")
                .into_iter()
                .collect();
        assert_eq!(result.len(), 2);
        assert!(result.contains_key("bogus"));
        assert_eq!(result.get("log_dir").unwrap(), "/tmp/x");
    }

    #[test]
    fn empty_input_yields_empty() {
        assert!(parse_kv("").is_empty());
        assert!(parse_kv("# only comments\n\n").is_empty());
    }
}
