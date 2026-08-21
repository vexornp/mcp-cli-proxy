use std::collections::HashMap;
use std::path::Path;

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
