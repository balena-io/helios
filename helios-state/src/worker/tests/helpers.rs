use crate::models::{Device, DeviceTarget};

use mahler::dag::{Dag, seq};
use mahler::worker::FindWorkflow;
use pretty_assertions::assert_eq;
use serde_json::Value;
use tracing_subscriber::fmt::{self, format::FmtSpan};
use tracing_subscriber::{EnvFilter, prelude::*};

pub(super) fn init_tracing() {
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

pub(super) fn assert_workflow(current: Value, target: Value, expected: Dag<&str>) {
    let current = serde_json::from_value::<Device>(current).unwrap();
    let target = serde_json::from_value::<DeviceTarget>(target).unwrap();
    let (_, workflow) = super::super::worker()
        .initial_state(current)
        .find_workflow(target)
        .unwrap();
    let workflow = workflow.expect("workflow should be found");
    let expected = expected
        + seq!(
            "clean-up host metadata and images",
            "clean-up app metadata and images"
        );
    assert_eq!(
        workflow.to_string(),
        expected.to_string(),
        "unexpected plan:\n{workflow}"
    );
}

/// Wraps a DAG with `prepare release` and `finish release` steps,
/// matching the pattern for updates to an existing release.
pub(super) fn release_update(
    release: &str,
    app: &str,
    inner: Dag<&'static str>,
) -> Dag<&'static str> {
    let prepare: &str = format!("prepare release '{release}' for app with uuid '{app}'").leak();
    let finish: &str = format!("finish release '{release}' for app with uuid '{app}'").leak();
    seq!(prepare) + inner + seq!(finish)
}

pub(super) fn running_container(id: &str) -> Value {
    serde_json::json!({
        "id": id,
        "status": "running",
        "created": "2026-02-11T15:03:43Z",
    })
}

pub(super) fn stopped_container(id: &str) -> Value {
    serde_json::json!({
        "id": id,
        "status": "stopped",
        "created": "2026-02-11T15:03:43Z",
    })
}
