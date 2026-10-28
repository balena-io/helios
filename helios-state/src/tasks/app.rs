use mahler::extract::{Args, Res, Target, View};
use mahler::job;
use mahler::state::Map;
use mahler::task::prelude::*;
use mahler::worker::{Uninitialized, Worker};

use crate::common_types::Uuid;
use crate::models::{App, AppTarget};
use crate::store::{self, DocumentStore};

/// Initialize the app and store its local data
fn create_app(
    maybe_app: View<Option<App>>,
    Target(tgt_app): Target<App>,
    Args(app_uuid): Args<Uuid>,
    store: Res<DocumentStore>,
) -> IO<App, store::Error> {
    enforce!(maybe_app.is_none(), "app already exists");

    let AppTarget {
        id,
        name,
        rejected_release,
        ..
    } = tgt_app;
    let app = maybe_app.create(App {
        id,
        name,
        locked: false,
        lockfiles: Vec::new(),
        releases: Map::new(),
        rejected_release,
    });

    with_io(app, async move |app| {
        let local_store = store.as_ref().expect("store should be available");
        // store id and name as local state
        local_store
            .put(format!("apps/{app_uuid}/id"), &app.id)
            .await?;
        local_store
            .put(format!("apps/{app_uuid}/name"), &app.name)
            .await?;

        Ok(app)
    })
}

/// Remove an empty app
fn remove_app(mut app: View<Option<App>>) -> View<Option<App>> {
    if app
        .as_ref()
        .map(|a| !a.locked && a.releases.is_empty())
        .unwrap_or_default()
    {
        app.take();
    }
    app
}

/// Set or clear the app's rejected release flag.
///
/// The field is in-memory only — it tracks the target's rejection state
/// so the planner can preserve the current release via an exception and
/// so the report layer can surface the rejected release uuid.
fn set_rejected_release(
    mut current: View<Option<Uuid>>,
    Target(target): Target<Option<Uuid>>,
) -> View<Option<Uuid>> {
    *current = target;
    current
}

/// Update the local app id
fn store_app_id(
    mut id: View<u32>,
    Target(tgt): Target<u32>,
    Args(app_uuid): Args<String>,
    store: Res<DocumentStore>,
) -> IO<u32, store::Error> {
    *id = tgt;
    with_io(id, async move |id| {
        let local_store = store.as_ref().expect("store should be available");
        local_store.put(format!("apps/{app_uuid}/id"), &*id).await?;
        Ok(id)
    })
}

/// Update the local app name
fn store_app_name(
    mut name: View<Option<String>>,
    Target(tgt): Target<Option<String>>,
    Args(app_uuid): Args<String>,
    store: Res<DocumentStore>,
) -> IO<Option<String>, store::Error> {
    *name = tgt;
    with_io(name, async move |name| {
        let local_store = store.as_ref().expect("store should be available");
        local_store
            .put(format!("apps/{app_uuid}/name"), &*name)
            .await?;
        Ok(name)
    })
}

/// Update worker with the tasks managing the app entries
///
/// These do not touch releases, so an app can be created, rejected and
/// removed without user app support.
pub fn with_app_tasks<O>(worker: Worker<O, Uninitialized>) -> Worker<O, Uninitialized> {
    worker
        .jobs(
            "/apps/{app_uuid}",
            [
                job::create(create_app).with_description(|Args(uuid): Args<Uuid>| {
                    format!("initialize app with uuid '{uuid}'")
                }),
                job::delete(remove_app).with_description(|Args(uuid): Args<Uuid>| {
                    format!("remove app with uuid '{uuid}'")
                }),
            ],
        )
        .jobs(
            "/apps/{app_uuid}/name",
            [
                job::create(store_app_name).with_description(|Args(uuid): Args<Uuid>| {
                    format!("store name for app with uuid '{uuid}'")
                }),
                job::update(store_app_name).with_description(|Args(uuid): Args<Uuid>| {
                    format!("store name for app with uuid '{uuid}'")
                }),
            ],
        )
        .job(
            "/apps/{app_uuid}/id",
            job::update(store_app_id).with_description(|Args(uuid): Args<Uuid>| {
                format!("store id for app with uuid '{uuid}'")
            }),
        )
        .jobs(
            "/apps/{app_uuid}/rejected_release",
            [job::any(set_rejected_release).with_description(
                |Args(uuid): Args<Uuid>, Target(commit): Target<Option<Uuid>>| {
                    format!(
                        "set rejected target release for app '{uuid}' to {}",
                        commit.as_ref().map(|c| c.as_str()).unwrap_or("none")
                    )
                },
            )],
        )
}
