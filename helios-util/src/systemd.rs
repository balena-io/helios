use std::path::{Path, PathBuf};

use zbus::Connection;
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("failed to start transient unit: {0}")]
    StartUnit(String),
    #[error("stream terminated unexpectedly")]
    StreamEnded,
    #[error("command failed with code: {0}")]
    ExitStatus(i32),
    #[error("D-Bus error: {0}")]
    DBus(#[from] zbus::Error),
}

// systemd Manager D-Bus interface
#[zbus::proxy(
    interface = "org.freedesktop.systemd1.Manager",
    default_service = "org.freedesktop.systemd1",
    default_path = "/org/freedesktop/systemd1"
)]
trait Manager {
    /// StartTransientUnit method
    fn start_transient_unit(
        &self,
        name: &str,
        mode: &str,
        properties: Vec<(&str, Value<'_>)>,
        aux: Vec<(&str, Vec<(&str, Value<'_>)>)>,
    ) -> zbus::Result<OwnedObjectPath>;

    /// GetUnit method - get the object path for a unit by name
    fn get_unit(&self, name: &str) -> zbus::Result<OwnedObjectPath>;

    /// StopUnit method - stop a unit
    fn stop_unit(&self, name: &str, mode: &str) -> zbus::Result<OwnedObjectPath>;

    /// ResetFailedUnit method - reset the failed state of a unit
    fn reset_failed_unit(&self, name: &str) -> zbus::Result<()>;
}

// systemd Service D-Bus interface
#[zbus::proxy(
    interface = "org.freedesktop.systemd1.Service",
    default_service = "org.freedesktop.systemd1"
)]
trait Service {
    /// ExecMainStatus property - exit code of the main process
    #[zbus(property)]
    fn exec_main_status(&self) -> zbus::Result<i32>;
}

pub struct Command {
    cmd: String,
    args: Vec<String>,
    workdir: Option<PathBuf>,
}

impl Command {
    pub fn new<S: AsRef<str>>(cmd: S) -> Self {
        Self {
            cmd: cmd.as_ref().to_owned(),
            args: Vec::new(),
            workdir: None,
        }
    }

    pub fn args<S: AsRef<str>>(mut self, args: &[S]) -> Self {
        self.args = args.iter().map(|s| s.as_ref().to_string()).collect();
        self
    }

    pub fn workdir<P: AsRef<Path>>(mut self, dir: P) -> Self {
        self.workdir = Some(dir.as_ref().to_path_buf());
        self
    }
}

/// Helper to read a Unit property directly from D-Bus (bypasses zbus cache)
async fn get_unit_property<T: TryFrom<OwnedValue, Error = zbus::zvariant::Error>>(
    connection: &Connection,
    unit_path: &ObjectPath<'_>,
    property: &str,
) -> Result<T, Error> {
    let value: OwnedValue = connection
        .call_method(
            Some("org.freedesktop.systemd1"),
            unit_path,
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &("org.freedesktop.systemd1.Unit", property),
        )
        .await?
        .body()
        .deserialize()?;

    let converted = value
        .try_into()
        .map_err(|e: zbus::zvariant::Error| zbus::Error::from(e))?;

    Ok(converted)
}

/// Runs a transient systemd unit and waits for completion.
///
/// This function creates and starts a transient systemd service unit similar to
/// `systemd-run -wait`, executing the specified command and blocking until it completes.
///
/// # Example
///
/// ```no_run
/// use helios_util::systemd::{run, Command};
///
/// #[tokio::main]
/// async fn main() -> Result<(), Box<dyn std::error::Error>> {
///     let cmd = Command::new("/usr/bin/bash")
///         .args(&["-c", "echo hello"])
///         .workdir("/tmp");
///
///     run("my-script", &cmd).await?;
///     Ok(())
/// }
/// ```
pub async fn run(unit: &str, command: &Command) -> Result<(), Error> {
    // Connect to the host system bus
    // The DBUS_SYSTEM_BUS_ADDRESS environment variable should be set to point
    // to the host's D-Bus socket (e.g., unix:path=/run/dbus/system_bus_socket)
    // which is typically bind-mounted from the host into containers
    let connection = Connection::system().await?;

    // Get the systemd manager proxy
    let manager = ManagerProxy::new(&connection).await?;

    // Build the full unit name
    let full_unit_name = format!("{unit}.service");

    // Build the properties for the transient unit
    // ExecStart format: array of (path, argv, ignore_failure)
    // argv must include argv[0] (the command path)
    let mut argv = vec![command.cmd.clone()];
    argv.extend(command.args.clone());

    let mut properties: Vec<(&str, Value)> = vec![
        ("Type", Value::new("exec")),
        (
            "ExecStart",
            Value::new(vec![(command.cmd.clone(), argv, false)]),
        ),
        ("RemainAfterExit", Value::new(false)),
        ("CollectMode", Value::new("inactive")),
    ];

    // Add WorkingDirectory if specified
    if let Some(working_dir) = &command.workdir {
        properties.push((
            "WorkingDirectory",
            Value::new(working_dir.to_string_lossy().to_string()),
        ));
    }

    // Start the transient unit
    let start_job_path = manager
        .start_transient_unit(&full_unit_name, "fail", properties, vec![])
        .await
        .map_err(|e| Error::StartUnit(e.to_string()))?;

    if start_job_path.to_string() == "/" {
        return Err(Error::StartUnit(
            "null path received for starting unit".to_string(),
        ));
    }

    // Get the unit's object path
    let unit_path = manager.get_unit(&full_unit_name).await?;

    // Create proxy for the service (to read exit code later)
    let service_proxy = ServiceProxy::builder(&connection)
        .path(&unit_path)?
        .build()
        .await?;

    // Read initial state
    let mut active_state: String =
        get_unit_property(&connection, &unit_path, "ActiveState").await?;

    // Poll for unit completion (just wait for ActiveState to become inactive/failed)
    // With Type=exec, the start job completes immediately after fork+execve, so we only
    // need to monitor ActiveState transitions from "active" to "inactive"/"failed"
    // We use polling here as signal streams did not provide reliable results.
    use tokio::time::{Duration, interval};
    let mut poll_interval = interval(Duration::from_millis(100));

    loop {
        // Check if we're done
        if active_state == "inactive" || active_state == "failed" {
            break;
        }

        // Wait before polling again
        poll_interval.tick().await;

        // Read current ActiveState
        let new_active_state: String =
            get_unit_property(&connection, &unit_path, "ActiveState").await?;

        // Track state changes
        if new_active_state != active_state {
            active_state = new_active_state;
        }
    }

    // Get the exit code from the service
    let exit_code = service_proxy.exec_main_status().await?;

    // Reset the failed state to allow reuse of the same unit name
    let _ = manager.reset_failed_unit(&full_unit_name).await;

    if exit_code != 0 {
        return Err(Error::ExitStatus(exit_code));
    }
    Ok(())
}

/// Stops a systemd unit by name.
///
/// This function attempts to stop a systemd unit. If the unit is already stopped
/// or doesn't exist, it will not fail - it returns Ok(()) in all cases.
///
/// # Example
///
/// ```no_run
/// use helios_util::systemd;
///
/// #[tokio::main]
/// async fn main() -> Result<(), Box<dyn std::error::Error>> {
///     systemd::stop("my-script").await?;
///     Ok(())
/// }
/// ```
pub async fn stop(unit: &str) -> Result<(), Error> {
    let connection = Connection::system().await?;
    let manager = ManagerProxy::new(&connection).await?;
    let full_unit_name = format!("{unit}.service");

    // Stop the unit with replace mode to cancel any pending jobs
    let _ = manager.stop_unit(&full_unit_name, "replace").await;

    Ok(())
}
