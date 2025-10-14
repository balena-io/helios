use helios_util::systemd::{self, Command};

use tracing_subscriber::fmt::{self, format::FmtSpan};
use tracing_subscriber::{EnvFilter, prelude::*};

fn before() {
    // Initialize tracing subscriber with custom formatting
    tracing_subscriber::registry()
        .with(EnvFilter::from_default_env())
        .with(
            fmt::layer()
                .with_writer(std::io::stderr)
                .with_span_events(FmtSpan::CLOSE)
                .event_format(fmt::format().pretty().with_target(false)),
        )
        .try_init()
        .unwrap_or(());
}

#[tokio::test]
async fn test_systemd_run_long_command() {
    before();
    let cmd = Command::new("/usr/bin/sleep").args(&["2"]);
    let result = systemd::run("test-sleep", &cmd).await;

    assert!(result.is_ok(), "systemd run should succeed");
}

#[tokio::test]
async fn test_systemd_run_simple_command() {
    before();
    let cmd = Command::new("/usr/bin/echo").args(&["hello", "world"]);
    let result = systemd::run("test-echo", &cmd).await;

    assert!(result.is_ok(), "systemd run should succeed");
}

#[tokio::test]
async fn test_systemd_run_with_workdir() {
    before();
    let cmd = Command::new("/usr/bin/pwd").workdir("/tmp");

    let result = systemd::run("test-pwd", &cmd).await;
    assert!(result.is_ok(), "systemd run with workdir should succeed");
}

#[tokio::test]
async fn test_systemd_run_failing_command() {
    before();
    let cmd = Command::new("/bin/false");

    let result = systemd::run("test-fail", &cmd).await;
    assert!(result.is_err(), "systemd run should fail for non-zero exit");

    match result {
        Err(helios_util::systemd::SystemdError::ExitStatus(code)) => {
            assert_eq!(code, 1, "exit code should be 1");
        }
        Err(e) => panic!("expected ExitStatus error, got {e}"),
        _ => unreachable!(),
    }
}

#[tokio::test]
async fn test_systemd_run_with_args() {
    before();
    let cmd = Command::new("/bin/sh").args(&["-c", "exit 42"]);

    let result = systemd::run("test-exit-code", &cmd).await;
    assert!(result.is_err(), "command with non-zero exit should fail");

    match result {
        Err(helios_util::systemd::SystemdError::ExitStatus(code)) => {
            assert_eq!(code, 42, "exit code should match");
        }
        Err(e) => panic!("expected ExitStatus error, got {e}"),
        _ => unreachable!(),
    }
}

#[tokio::test]
async fn test_systemd_run_nonexistent_command() {
    before();
    let cmd = Command::new("/nonexistent/command");

    let result = systemd::run("test-nonexistent", &cmd).await;
    assert!(result.is_err(), "nonexistent command should fail");
}

#[tokio::test]
async fn test_systemd_stop_nonexistent_unit() {
    before();
    // Try to stop a unit that doesn't exist
    let result = systemd::stop("test-nonexistent-unit").await;

    // Should succeed (not fail)
    assert!(result.is_ok(), "stopping nonexistent unit should not fail");
}

#[tokio::test]
async fn test_systemd_stop_already_stopped_unit() {
    before();
    // First run a command that completes quickly
    let cmd = Command::new("/usr/bin/echo").args(&["test"]);
    let result = systemd::run("test-already-stopped", &cmd).await;
    assert!(result.is_ok(), "initial run should succeed");

    // Wait a bit for the unit to become inactive
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    // Try to stop it again when it's already stopped
    let result = systemd::stop("test-already-stopped").await;

    // Should succeed (not fail)
    assert!(
        result.is_ok(),
        "stopping already stopped unit should not fail"
    );
}

#[tokio::test]
async fn test_systemd_stop_running_unit() {
    before();
    // Start a long-running command
    let cmd = Command::new("/bin/sh").args(&["-c", "sleep 10"]);

    // Start the unit in a background task
    let handle = tokio::spawn(async move { systemd::run("test-stop-running", &cmd).await });

    // Wait a bit to ensure the unit is running
    tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;

    // Stop the running unit
    let stop_result = systemd::stop("test-stop-running").await;
    assert!(stop_result.is_ok(), "stopping running unit should succeed");

    // The original run should complete (possibly with an error since it was stopped)
    handle.await.unwrap().unwrap();
}
