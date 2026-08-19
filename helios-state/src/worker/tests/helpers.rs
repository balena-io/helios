use crate::models::{Device, DeviceTarget};

use mahler::dag::{Dag, seq};
use mahler::worker::{FindPlan, Workflow};
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

pub(super) fn assert_workflow(current: Value, target: Value, expected: Dag<&str>) -> Workflow {
    let current = serde_json::from_value::<Device>(current).unwrap();
    let target = serde_json::from_value::<DeviceTarget>(target).unwrap();
    let (_, workflow) = super::super::worker()
        .initial_state(current)
        .find_plan(target)
        .unwrap();
    let workflow = workflow.expect("workflow should be found");
    assert_eq!(
        workflow.to_string(),
        expected.to_string(),
        "unexpected plan:\n{workflow}"
    );

    workflow
}

/// Assert the planner skips the operation on `path` with the given reason and
/// finds no other work to do. Used for targets the planner cannot reach, e.g. a
/// service start gated on a `depends_on` condition that has terminally failed.
pub(super) fn assert_exception(current: Value, target: Value, path: &str, reason: &str) {
    let workflow = assert_workflow(current, target, Dag::new([]));
    let exceptions = workflow.exceptions();
    assert_eq!(
        exceptions.len(),
        1,
        "expected a single exception, found {exceptions:?}"
    );
    assert_eq!(exceptions[0].operation.path().as_str(), path);
    assert_eq!(exceptions[0].reason.as_deref(), Some(reason));
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

pub(super) fn running_container(name: &str) -> Value {
    serde_json::json!({
        "name": name,
        "status": "running",
        "created": "2026-02-11T15:03:43Z",
    })
}

pub(super) fn stopped_container(name: &str) -> Value {
    serde_json::json!({
        "name": name,
        "status": "stopped",
        "created": "2026-02-11T15:03:43Z",
    })
}

pub(super) fn created_container(name: &str) -> Value {
    serde_json::json!({
        "name": name,
        "status": "created",
        "created": "2026-02-11T15:03:43Z",
    })
}
