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

/// Assert the planner cannot reach the target at all. Used for targets made
/// unreachable by state the planner cannot act on, e.g. a `depends_on`
/// condition that has terminally failed.
pub(super) fn assert_no_workflow(current: Value, target: Value) {
    let current = serde_json::from_value::<Device>(current).unwrap();
    let target = serde_json::from_value::<DeviceTarget>(target).unwrap();
    let (_, workflow) = super::super::worker()
        .initial_state(current)
        .find_workflow(target)
        .unwrap();
    assert!(
        workflow.is_none(),
        "expected no workflow, found:\n{}",
        workflow.unwrap()
    );
}

/// Assert that the planner finds a workflow with no tasks in it. Unlike
/// [`assert_no_workflow`], the target is reachable: the planner simply has
/// nothing left to do, or has deferred every divergence it found.
pub(super) fn assert_empty_workflow(current: Value, target: Value) {
    let current = serde_json::from_value::<Device>(current).unwrap();
    let target = serde_json::from_value::<DeviceTarget>(target).unwrap();
    let (_, workflow) = super::super::worker()
        .initial_state(current)
        .find_workflow(target)
        .unwrap();
    let workflow = workflow.expect("workflow should be found");
    assert_eq!(
        workflow.to_string(),
        Dag::<&str>::default().to_string(),
        "expected an empty plan, got:\n{workflow}"
    );
}

/// Assert that the planner rules out every pending change for the given
/// current/target pair because an exception matched, and that the skip is
/// reported with `reason`.
pub(super) fn assert_aborted(current: Value, target: Value, reason: &str) {
    let current = serde_json::from_value::<Device>(current).unwrap();
    let target = serde_json::from_value::<DeviceTarget>(target).unwrap();
    let (_, workflow) = super::super::worker()
        .initial_state(current)
        .find_workflow(target)
        .unwrap();
    let workflow = workflow.expect("workflow should be found");
    assert_eq!(
        workflow.to_string(),
        Dag::<&str>::default().to_string(),
        "expected no tasks to be planned, got:\n{workflow}"
    );

    let reasons: Vec<&str> = workflow
        .exceptions()
        .iter()
        .filter_map(|ignored| ignored.reason.as_deref())
        .collect();
    assert!(
        reasons.contains(&reason),
        "expected a skipped operation with reason '{reason}', got: {reasons:?}"
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
