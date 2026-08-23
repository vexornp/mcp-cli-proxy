use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};

use mcp_cli_proxy::bridge::{connect, DaemonOptions};
use mcp_cli_proxy::daemon::run_daemon;
use mcp_cli_proxy::exec::{ExecConfig, ExecParams, Executor};

fn ephemeral_addr() -> SocketAddr {
    SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 1), 0))
}

async fn spawn_daemon() -> (tokio::task::JoinHandle<()>, SocketAddr) {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let opts = DaemonOptions {
        addr: ephemeral_addr(),
        config: ExecConfig::defaults(),
    };
    let handle = tokio::spawn(async move {
        let _ = run_daemon(opts, Some(tx)).await;
    });
    let bound = rx.await.expect("daemon did not report bound addr");
    (handle, bound)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_runs_echo_command() {
    let (daemon, addr) = spawn_daemon().await;

    let exec = connect(addr).await.unwrap();
    let params = ExecParams {
        command: "echo hi".into(),
        ..Default::default()
    };
    let result = exec.exec(params).await.unwrap();
    assert_eq!(result.stdout, "hi\n");
    assert_eq!(result.exit_code, Some(0));
    assert!(!result.timed_out);

    daemon.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_reports_nonzero_exit() {
    let (daemon, addr) = spawn_daemon().await;

    let exec = connect(addr).await.unwrap();
    let params = ExecParams {
        command: "exit 7".into(),
        ..Default::default()
    };
    let result = exec.exec(params).await.unwrap();
    assert_eq!(result.exit_code, Some(7));

    daemon.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_enforces_timeout() {
    let (daemon, addr) = spawn_daemon().await;

    let exec = connect(addr).await.unwrap();
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
}
