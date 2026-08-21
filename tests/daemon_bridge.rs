use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use mcp_cli_proxy::bridge::{connect, DaemonOptions};
use mcp_cli_proxy::daemon::run_daemon;
use mcp_cli_proxy::exec::{ExecConfig, ExecParams, Executor};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_socket() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let mut p = std::env::temp_dir();
    p.push(format!(
        "mcp-cli-proxy-test-{}-{}.sock",
        std::process::id(),
        n
    ));
    let _ = std::fs::remove_file(&p);
    p
}

async fn wait_for_socket(path: &std::path::Path) {
    for _ in 0..200 {
        if path.exists() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("daemon did not create socket in time: {}", path.display());
}

#[ignore = "requires unsandboxed env (sandbox blocks UnixListener::bind); run with: cargo test --test daemon_bridge -- --ignored"]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_runs_echo_command() {
    let sock = temp_socket();
    let opts = DaemonOptions {
        socket_path: sock.clone(),
        config: ExecConfig::defaults(),
    };
    let daemon = tokio::spawn(async move { run_daemon(opts).await });

    wait_for_socket(&sock).await;

    let exec = connect(&sock).await.unwrap();
    let params = ExecParams {
        command: "echo hi".into(),
        ..Default::default()
    };
    let result = exec.exec(params).await.unwrap();
    assert_eq!(result.stdout, "hi\n");
    assert_eq!(result.exit_code, Some(0));
    assert!(!result.timed_out);

    daemon.abort();
    let _ = std::fs::remove_file(&sock);
}

#[ignore = "requires unsandboxed env (sandbox blocks UnixListener::bind); run with: cargo test --test daemon_bridge -- --ignored"]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_reports_nonzero_exit() {
    let sock = temp_socket();
    let opts = DaemonOptions {
        socket_path: sock.clone(),
        config: ExecConfig::defaults(),
    };
    let daemon = tokio::spawn(async move { run_daemon(opts).await });

    wait_for_socket(&sock).await;

    let exec = connect(&sock).await.unwrap();
    let params = ExecParams {
        command: "exit 7".into(),
        ..Default::default()
    };
    let result = exec.exec(params).await.unwrap();
    assert_eq!(result.exit_code, Some(7));

    daemon.abort();
    let _ = std::fs::remove_file(&sock);
}

#[ignore = "requires unsandboxed env (sandbox blocks UnixListener::bind); run with: cargo test --test daemon_bridge -- --ignored"]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_enforces_timeout() {
    let sock = temp_socket();
    let opts = DaemonOptions {
        socket_path: sock.clone(),
        config: ExecConfig::defaults(),
    };
    let daemon = tokio::spawn(async move { run_daemon(opts).await });

    wait_for_socket(&sock).await;

    let exec = connect(&sock).await.unwrap();
    let params = ExecParams {
        command: "sleep 10".into(),
        timeout_secs: Some(1),
        ..Default::default()
    };
    let start = std::time::Instant::now();
    let result = exec.exec(params).await.unwrap();
    assert!(result.timed_out);
    assert_eq!(result.exit_code, None);
    assert!(
        start.elapsed().as_secs() < 5,
        "should be killed near 1s, got {:?}",
        start.elapsed()
    );

    daemon.abort();
    let _ = std::fs::remove_file(&sock);
}
