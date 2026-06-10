use helios_oci::Client;
use helios_util::systemd;
use tracing::{debug, info, instrument, warn};

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
const rows = await query("SELECT value FROM config WHERE key='listenPortOverride'");
if (rows.length > 0 && rows[0].value === process.env.PORT_OVERRIDE) {
  console.log('false');
  process.exit(0);
}
await query(
  `INSERT INTO config (key, value) VALUES ('apiEndpointOverride', '${process.env.HOST_OVERRIDE}') ON CONFLICT(key) DO UPDATE SET value=excluded.value`,
);
await query(
  `INSERT INTO config (key, value) VALUES ('listenPortOverride', '${process.env.PORT_OVERRIDE}') ON CONFLICT(key) DO UPDATE SET value=excluded.value`,
);
console.log('true');
"#;

/// Candidate legacy supervisor container names, in priority order.
const SUPERVISOR_NAMES: [&str; 2] = ["balena_supervisor", "resin_supervisor"];

/// Override values written verbatim to the legacy supervisor's config DB.
///
/// Provided as input by the user
pub struct TakeoverConfig {
    /// Written verbatim to the supervisor's `apiEndpointOverride`.
    pub host_override: String,
    /// Written verbatim to the supervisor's `listenPortOverride`; also the
    /// idempotency key.
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
        warn!("no legacy supervisor container found; nothing to take over");
        return Ok(TakeoverOutcome::NotPresent);
    };

    // The systemd unit name uses dashes where the container name uses
    // underscores (`balena_supervisor` -> `balena-supervisor`).
    let unit = supervisor.name.replace('_', "-");

    debug!(container = %supervisor.name, "configuring legacy supervisor for takeover");

    let host_env = format!("HOST_OVERRIDE={}", cfg.host_override);
    let port_env = format!("PORT_OVERRIDE={}", cfg.port_override);

    let output = container
        .exec(
            &supervisor.name,
            &["node", "--input-type=module", "-e", TAKEOVER_SCRIPT],
            &[&host_env, &port_env],
        )
        .await?;

    if output.exit_code != 0 {
        return Err(TakeoverError::Exec {
            code: output.exit_code,
            stderr: output.stderr,
        });
    }

    match output.stdout.trim() {
        "false" => {
            debug!("legacy supervisor already configured for takeover");
            Ok(TakeoverOutcome::AlreadyConfigured)
        }
        "true" => {
            info!(%unit, "restarting legacy supervisor to apply takeover");
            systemd::restart(&unit).await?;
            Ok(TakeoverOutcome::Migrated)
        }
        other => Err(TakeoverError::Exec {
            code: output.exit_code,
            stderr: format!(
                "unexpected supervisor output: {other:?} (stderr: {})",
                output.stderr
            ),
        }),
    }
}
