use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

use mcp_cli_proxy::exec::{run_command, ExecConfig, ExecParams};

#[tokio::test]
async fn runs_simple_command() {
    let params = ExecParams {
        command: "echo hello".into(),
        ..Default::default()
    };
    let result = run_command(params, ExecConfig::defaults()).await.unwrap();
    assert_eq!(result.exit_code, Some(0));
    assert_eq!(result.stdout, "hello\n");
    assert!(!result.timed_out);
}

#[tokio::test]
async fn captures_stderr_separately() {
    let params = ExecParams {
        command: "echo out; echo err >&2".into(),
        ..Default::default()
    };
    let result = run_command(params, ExecConfig::defaults()).await.unwrap();
    assert_eq!(result.stdout, "out\n");
    assert_eq!(result.stderr, "err\n");
}

#[tokio::test]
async fn nonzero_exit() {
    let params = ExecParams {
        command: "false".into(),
        ..Default::default()
    };
    let result = run_command(params, ExecConfig::defaults()).await.unwrap();
    assert_eq!(result.exit_code, Some(1));
}

#[tokio::test]
async fn command_not_found_exit_code() {
    let params = ExecParams {
        command: "this_does_not_exist_xyz".into(),
        ..Default::default()
    };
    let result = run_command(params, ExecConfig::defaults()).await.unwrap();
    assert_eq!(result.exit_code, Some(127));
}

#[tokio::test]
async fn pipes_and_globs_work() {
    // sh -c handles pipes natively; confirms shell-string execution model.
    let params = ExecParams {
        command: "printf 'a\nb\nc\n' | grep a".into(),
        ..Default::default()
    };
    let result = run_command(params, ExecConfig::defaults()).await.unwrap();
    assert_eq!(result.stdout, "a\n");
}

#[tokio::test]
async fn pipes_stdin() {
    let params = ExecParams {
        command: "cat".into(),
        stdin: Some("hi\n".into()),
        ..Default::default()
    };
    let result = run_command(params, ExecConfig::defaults()).await.unwrap();
    assert_eq!(result.stdout, "hi\n");
}

#[tokio::test]
async fn respects_cwd() {
    let cwd = std::env::temp_dir();
    let params = ExecParams {
        command: "pwd".into(),
        cwd: Some(cwd.to_string_lossy().into_owned()),
        ..Default::default()
    };
    let result = run_command(params, ExecConfig::defaults()).await.unwrap();
    let reported = Path::new(result.stdout.trim());
    let canonical_reported = std::fs::canonicalize(reported).unwrap();
    let canonical_set = std::fs::canonicalize(&cwd).unwrap();
    assert_eq!(canonical_reported, canonical_set);
}

#[tokio::test]
async fn passes_env_overrides() {
    let mut env = HashMap::new();
    env.insert("MY_PROXY_VAR".to_string(), "hello".to_string());
    let params = ExecParams {
        command: "printenv MY_PROXY_VAR".into(),
        env: Some(env),
        ..Default::default()
    };
    let result = run_command(params, ExecConfig::defaults()).await.unwrap();
    assert_eq!(result.stdout, "hello\n");
}

#[tokio::test]
async fn timeout_kills_and_reports() {
    let params = ExecParams {
        command: "sleep 10".into(),
        timeout_secs: Some(1),
        ..Default::default()
    };
    let start = Instant::now();
    let result = run_command(params, ExecConfig::defaults()).await.unwrap();
    assert!(result.timed_out);
    assert_eq!(result.exit_code, None);
    assert!(start.elapsed().as_secs() < 5, "should be killed near 1s, not 10s");
}

#[tokio::test]
async fn timeout_clamped_to_max() {
    // timeout_secs (3600) exceeds max_timeout_secs (2) -> clamped to 2.
    let config = ExecConfig {
        max_timeout_secs: 2,
        ..ExecConfig::defaults()
    };
    let params = ExecParams {
        command: "sleep 10".into(),
        timeout_secs: Some(3600),
        ..Default::default()
    };
    let start = Instant::now();
    let result = run_command(params, config).await.unwrap();
    assert!(result.timed_out);
    assert_eq!(result.exit_code, None);
    let elapsed = start.elapsed().as_secs();
    assert!(elapsed >= 2 && elapsed < 8, "should be clamped to ~2s, got {elapsed}s");
}

#[tokio::test]
async fn output_truncation() {
    let config = ExecConfig {
        output_cap_bytes: 16,
        ..ExecConfig::defaults()
    };
    let params = ExecParams {
        command: "seq 1 1000".into(),
        ..Default::default()
    };
    let result = run_command(params, config).await.unwrap();
    assert_eq!(result.exit_code, Some(0));
    assert!(result.stdout_truncated);
    assert_eq!(result.stdout.len(), 16);
}

#[tokio::test]
async fn signal_exit_returns_128_plus_signal() {
    // sh kills itself with SIGTERM (signal 15) -> exit_code should be 128+15=143.
    let params = ExecParams {
        command: "kill -TERM $$".into(),
        ..Default::default()
    };
    let result = run_command(params, ExecConfig::defaults()).await.unwrap();
    assert!(!result.timed_out);
    assert_eq!(result.exit_code, Some(143));
}

#[tokio::test]
async fn large_stdin_to_non_reading_command_does_not_hang() {
    // A command that ignores stdin, with a large input that exceeds the pipe
    // buffer. Without the stdin-write timeout fix, this would deadlock.
    let large_input = "x".repeat(100_000);
    let params = ExecParams {
        command: "true".into(),
        stdin: Some(large_input),
        timeout_secs: Some(5),
        ..Default::default()
    };
    let start = std::time::Instant::now();
    let result = run_command(params, ExecConfig::defaults()).await.unwrap();
    assert!(!result.timed_out, "should complete, not time out");
    assert_eq!(result.exit_code, Some(0));
    assert!(
        start.elapsed().as_secs() < 10,
        "should complete near-instantly, not hang"
    );
}
