//! Log of planning failures, recording the current and target states the
//! planner could not find a workflow between.
//!
//! The log is a [JSON Lines](https://jsonlines.org/) file, one record per
//! failure, rotating to a single backup once it reaches [`MAX_FILE_SIZE`].
//! As the size is checked before each write, the record that triggers the
//! rotation can take a file over the limit.

use std::io::{self, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};

use chrono::{SecondsFormat, Utc};
use rolling_file::{BasicRollingFileAppender, RollingConditionBasic};
use serde::Serialize;
use tracing::warn;

use crate::util::fs::run_async;

use super::models::{Device, DeviceTarget};

/// Name of the log file
const FILE_NAME: &str = "planning-failures.jsonl";

/// Size at which the log file rotates
const MAX_FILE_SIZE: u64 = 512 * 1024;

#[derive(Serialize)]
struct Record<'a> {
    /// RFC 3339 timestamp, in UTC
    timestamp: String,
    /// Version of helios that failed to plan
    version: &'static str,
    /// The planning error, if planning failed with one rather than finding
    /// no workflow
    error: Option<String>,
    /// The local (current) state
    local: &'a Device,
    /// The target state given to the planner
    target: &'a DeviceTarget,
}

pub struct PlanningLog(Arc<Mutex<BasicRollingFileAppender>>);

impl PlanningLog {
    /// Open the log in the given directory, which must exist
    pub async fn open(dir: &Path) -> io::Result<Self> {
        let path = dir.join(FILE_NAME);
        let appender = run_async(move || {
            // Unbuffered, so every record reaches the file once written
            BasicRollingFileAppender::new_with_buffer_capacity(
                path,
                RollingConditionBasic::new().max_size(MAX_FILE_SIZE),
                1,
                0,
            )
        })
        .await?;
        Ok(Self(Arc::new(Mutex::new(appender))))
    }

    /// Append a planning failure to the log
    ///
    /// Failing to write the record is logged, so it does not interrupt the
    /// caller.
    pub async fn record(
        &self,
        current: &Device,
        target: &DeviceTarget,
        error: Option<&mahler::error::Error>,
    ) {
        if let Err(e) = append(self.0.clone(), current, target, error).await {
            warn!("failed to record planning failure: {e}");
        }
    }
}

async fn append(
    appender: Arc<Mutex<BasicRollingFileAppender>>,
    local: &Device,
    target: &DeviceTarget,
    error: Option<&mahler::error::Error>,
) -> io::Result<()> {
    let mut line = serde_json::to_vec(&Record {
        timestamp: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
        version: env!("CARGO_PKG_VERSION"),
        error: error.map(|e| e.to_string()),
        local,
        target,
    })?;
    line.push(b'\n');

    run_async(move || {
        // The appender holds no invariant a panicking writer could break
        let mut appender = appender.lock().unwrap_or_else(|e| e.into_inner());
        appender.write_all(&line)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[tokio::test]
    async fn it_appends_one_record_per_line() {
        let dir = tempfile::tempdir().unwrap();
        let log = PlanningLog::open(dir.path()).await.unwrap();
        let current: Device = serde_json::from_value(serde_json::json!({
            "uuid": "device-uuid",
        }))
        .unwrap();
        let target = DeviceTarget::from(current.clone());

        log.record(&current, &target, None).await;
        log.record(&current, &target, None).await;

        let records: Vec<Value> = std::fs::read_to_string(dir.path().join(FILE_NAME))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0]["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(records[0]["local"], serde_json::to_value(&current).unwrap());
        assert_eq!(records[0]["target"], serde_json::to_value(&target).unwrap());
        assert_eq!(records[0]["error"], Value::Null);
    }
}
