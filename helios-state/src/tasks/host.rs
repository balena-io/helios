use mahler::extract::{Res, Target, View};
use mahler::task::prelude::*;

use crate::models::{Host, HostTarget};
use crate::util::store::{Store, StoreError};

/// Initialize the hostapp and store the uuid
pub fn init_hostapp(
    mut opt_host: View<Option<Host>>,
    Target(tgt): Target<HostTarget>,
    store: Res<Store>,
) -> IO<Option<Host>, StoreError> {
    let HostTarget { uuid, .. } = tgt;
    opt_host.replace(Host {
        uuid,
        releases: Default::default(),
    });

    with_io(opt_host, async move |opt_host| {
        if let (Some(local_store), Some(host)) = (store.as_ref(), opt_host.as_ref()) {
            local_store.write("/host", "uuid", &host.uuid).await?;
        }
        Ok(opt_host)
    })
}

//
// fn update_hostapp(System(device): System(device), host: View<Option<Host>>) -> View<Option<Host>> {
//     // TODO:
//     // - Compare board revision
//     // if the current board revision is the same as the target one
//     // just update the app information on disk
//     // if the revisions are different, pull updater image and run the update script
//     todo!()
// }
//
