use clap::Args;
use tracing::{debug, info, instrument, warn};

use crate::oci::Client;
use crate::util::http::Uri;
use crate::util::{dirs, fs, systemd};

/// ES-module script (run via `node --input-type=module -e`) that idempotently
/// points the legacy supervisor at helios by writing its `apiEndpointOverride`
/// and `listenPortOverride` config keys. Prints `false` when no change was
/// needed, `true` when the DB was updated. Inputs come from the exec `Env`:
/// `HOST_OVERRIDE`, `PORT_OVERRIDE`.
const TAKEOVER_SCRIPT: &str = r#"
import sqlite3 from 'sqlite3';
const db = new sqlite3.Database('/data/database.sqlite');
const query = (s) =>
  new Promise((resolve, reject) =>
    db.all(s, (err, rows) => (err ? reject(err) : resolve(rows))));
const rows = await query(
  "SELECT key, value FROM config WHERE key IN ('apiEndpointOverride', 'listenPortOverride')");
const cur = Object.fromEntries(rows.map((r) => [r.key, r.value]));
if (cur.apiEndpointOverride === process.env.HOST_OVERRIDE
    && cur.listenPortOverride === process.env.PORT_OVERRIDE) {
  console.log('false');
  process.exit(0);
}
await query(
  `INSERT INTO config (key, value) VALUES
     ('apiEndpointOverride', '${process.env.HOST_OVERRIDE}'),
     ('listenPortOverride', '${process.env.PORT_OVERRIDE}')
   ON CONFLICT(key) DO UPDATE SET value=excluded.value`,
);
console.log('true');
"#;

/// Candidate legacy supervisor container names, in priority order.
const SUPERVISOR_NAMES: [&str; 2] = ["balena_supervisor", "resin_supervisor"];

/// Runtime-dir breadcrumb file marking a takeover whose restart has not yet completed.
const RESTART_PENDING_FLAG: &str = "helios-legacy-takeover-breadcrumb";

/// Override values written verbatim to the legacy supervisor's config DB.
#[derive(Clone, Debug, Args)]
pub struct TakeoverConfig {
    /// Api endpoint to write as the supervisor's `apiEndpointOverride`.
    #[arg(long = "override-host", value_name = "url")]
    pub host_override: Uri,
    /// Port configuration to write as the supervisor's `listenPortOverride`.
    #[arg(long = "override-port", value_name = "port")]
    pub port_override: u16,
}

/// Result of a takeover attempt.
pub enum TakeoverOutcome {
    /// No legacy supervisor container was found; nothing to do.
    NotPresent,
    /// The supervisor was already pointed at helios; no change made.
    AlreadyConfigured,
    /// The supervisor DB was updated and the unit restarted.
    Migrated,
}

#[derive(Debug, thiserror::Error)]
pub enum TakeoverError {
    #[error(transparent)]
    Oci(#[from] helios_oci::Error),
    #[error(transparent)]
    Systemd(#[from] systemd::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("supervisor exec failed (exit {code}): {stderr}")]
    Exec { code: i64, stderr: String },
}

/// Take over the legacy supervisor: point it at helios via its own config DB,
/// then restart it so it re-reads the override.
///
/// Idempotent: a supervisor already configured for takeover is left untouched.
#[instrument(name = "takeover", skip_all, err)]
pub async fn takeover(oci: &Client, cfg: TakeoverConfig) -> Result<TakeoverOutcome, TakeoverError> {
    let container = oci.non_namepaced_container();

    // Resolve the supervisor container, trying each candidate name in order.
    let mut supervisor = None;
    for name in SUPERVISOR_NAMES {
        match container.inspect(name).await {
            Ok(c) => {
                supervisor = Some(c);
                break;
            }
            Err(e) if e.is_not_found() => continue,
            Err(e) => return Err(e.into()),
        }
    }
    let Some(supervisor) = supervisor else {
        warn!("no legacy supervisor container found; nothing to do");
        return Ok(TakeoverOutcome::NotPresent);
    };

    // The systemd unit name uses dashes where the container name uses
    // underscores (`balena_supervisor` -> `balena-supervisor`).
    let unit = supervisor.name.replace('_', "-");

    debug!(container = %supervisor.name, "configuring legacy supervisor");

    let host_env = format!("HOST_OVERRIDE={}", cfg.host_override);
    let port_env = format!("PORT_OVERRIDE={}", cfg.port_override);

    // Create a flag in the runtime dir to detect a pending restart in case of a crash
    let restart_pending = !set_restart_flag().await?;

    let output = container
        .exec(
            &supervisor.name,
            &["node", "--input-type=module", "-e", TAKEOVER_SCRIPT],
            &[&host_env, &port_env],
        )
        .await;

    let output = match output {
        Ok(o) => o,
        Err(e) => {
            // remove the breadcrumb in this case to avoid an unnecessary supervisor restart
            // on the next run
            remove_restart_flag().await?;
            return Err(e.into());
        }
    };

    if output.exit_code != 0 {
        // remove the breadcrumb in this case to avoid an unnecessary supervisor restart
        // on the next run
        remove_restart_flag().await?;
        return Err(TakeoverError::Exec {
            code: output.exit_code,
            stderr: output.stderr,
        });
    }

    match output.stdout.trim() {
        // No takeover neede and no restart pending
        "false" if !restart_pending => {
            debug!("legacy supervisor already configured");

            // clear the flag
            remove_restart_flag().await?;
            Ok(TakeoverOutcome::AlreadyConfigured)
        }
        // A takeover just took place or a restart was pending
        "false" | "true" => {
            info!(%unit, "restarting legacy supervisor");
            systemd::restart(&unit).await?;

            // clear the flag
            remove_restart_flag().await?;
            Ok(TakeoverOutcome::Migrated)
        }
        // exit_code == 0, means only true/false responses are expected from the script
        other => unreachable!("expected true/false, got {other}"),
    }
}

/// Path to the restart-pending flag in the runtime dir.
fn restart_flag_path() -> std::path::PathBuf {
    dirs::runtime_dir().join(RESTART_PENDING_FLAG)
}

/// Create the restart-pending flag if it does not already
/// exist. Returns `true` when the flag was newly created.
async fn set_restart_flag() -> std::io::Result<bool> {
    fs::run_async(|| {
        dirs::ensure_runtime_dir()?;
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(restart_flag_path())
        {
            Ok(_) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
            Err(e) => Err(e),
        }
    })
    .await
}

/// Remove the restart-pending flag.
async fn remove_restart_flag() -> std::io::Result<()> {
    fs::run_async(|| match std::fs::remove_file(restart_flag_path()) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    })
    .await
}
