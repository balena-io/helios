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
        Err(helios_util::systemd::Error::ExitStatus(code)) => {
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
        Err(helios_util::systemd::Error::ExitStatus(code)) => {
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
async fn test_systemd_start_nonexistent_unit() {
    before();
    // Try to start a unit that doesn't exist
    let err = systemd::start("test-nonexistent-unit").await.unwrap_err();
    assert!(
        matches!(err, systemd::Error::NoSuchUnit(_)),
        "expected NoSuchUnit, got {err}"
    );
}

#[tokio::test]
async fn test_systemd_start_failing_unit() {
    before();
    // Create a unit that will fail and continue returning ActiveState `failed` on
    // subsequent restarts
    let cmd = Command::new("/nonexistent/command");
    let _ = systemd::run("test-start-failing", &cmd).await;

    // Starting a unit that fails to activate must return an error instead of
    // waiting for the `active` state forever.
    let result = tokio::time::timeout(
        tokio::time::Duration::from_secs(10),
        systemd::start("test-start-failing"),
    )
    .await;

    assert!(
        result.is_ok(),
        "start should not hang for a unit that fails to activate"
    );
    assert!(
        result.unwrap().is_err(),
        "start should error for a unit that fails to activate"
    );
}

#[tokio::test]
async fn test_systemd_restart_failing_unit() {
    before();
    // Create a unit that will fail and continue returning ActiveState `failed` on
    // subsequent restarts
    let cmd = Command::new("/nonexistent/command");
    let _ = systemd::run("test-restart-failing", &cmd).await;

    // Restarting a unit that fails to activate must return an error instead of
    // waiting for the `active` state forever.
    let result = tokio::time::timeout(
        tokio::time::Duration::from_secs(10),
        systemd::restart("test-restart-failing"),
    )
    .await;

    assert!(
        result.is_ok(),
        "restart should not hang for a unit that fails to activate"
    );
    assert!(
        result.unwrap().is_err(),
        "restart should error for a unit that fails to activate"
    );
}

#[tokio::test]
async fn test_systemd_start_running_unit() {
    before();
    // Start a long-running transient unit so a real, active unit exists.
    let cmd = Command::new("/bin/sh").args(&["-c", "sleep 10"]);
    let handle = tokio::spawn(async move { systemd::run("test-start-running", &cmd).await });

    // Wait for the unit to be up and running.
    tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;

    // Starting an already-active unit enqueues a no-op job that completes with
    // result "done", so start must succeed (exercises the success path).
    let result = tokio::time::timeout(
        tokio::time::Duration::from_secs(10),
        systemd::start("test-start-running"),
    )
    .await;

    assert!(result.is_ok(), "start should not hang for a running unit");
    assert!(
        result.unwrap().is_ok(),
        "start of an active unit should succeed"
    );

    // Clean up: stopping lets the background run() observe the unit go inactive.
    systemd::stop("test-start-running").await.unwrap();
    handle.await.unwrap().ok();
}

#[tokio::test]
async fn test_systemd_restart_running_unit() {
    before();
    // Start a long-running transient unit so a real, active unit exists.
    let cmd = Command::new("/bin/sh").args(&["-c", "sleep 10"]);
    let handle = tokio::spawn(async move { systemd::run("test-restart-running", &cmd).await });

    // Wait for the unit to be up and running.
    tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;

    // Restarting a healthy unit re-activates it; the restart job completes with
    // result "done", so restart must succeed.
    let result = tokio::time::timeout(
        tokio::time::Duration::from_secs(10),
        systemd::restart("test-restart-running"),
    )
    .await;

    assert!(result.is_ok(), "restart should not hang for a running unit");
    assert!(
        result.unwrap().is_ok(),
        "restart of an active unit should succeed"
    );

    // Clean up the (restarted) unit.
    systemd::stop("test-restart-running").await.unwrap();
    handle.await.ok();
}

#[tokio::test]
async fn test_systemd_stop_nonexistent_unit() {
    before();
    // Try to stop a unit that doesn't exist
    let err = systemd::stop("test-nonexistent-unit").await.unwrap_err();
    assert!(
        matches!(err, systemd::Error::NoSuchUnit(_)),
        "expected NoSuchUnit, got {err}"
    );
}

#[tokio::test]
async fn test_systemd_stop_already_stopped_unit() {
    before();
    // `helios-test-dummy` is a real (non-transient) unit predefined in the mock
    // via MOCK_SYSTEMD_UNITS. Unlike a transient unit it is not garbage-collected
    // when it becomes inactive, so it stays known to systemd after being stopped —
    // modelling a normal service unit.

    // Stop it once (it boots active) so it is inactive but still loaded.
    systemd::stop("helios-test-dummy")
        .await
        .expect("stopping a running unit should succeed");

    // Stopping a unit that is already stopped but still exists must succeed as a
    // no-op, in contrast to stopping a unit that does not exist at all.
    systemd::stop("helios-test-dummy")
        .await
        .expect("stopping an already-stopped unit should succeed");
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
    systemd::stop("test-stop-running").await.unwrap();

    // The original run should complete (possibly with an error since it was stopped)
    handle.await.unwrap().unwrap();
}
