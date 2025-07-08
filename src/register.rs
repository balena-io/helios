use std::convert::Infallible;

use crate::cli::RemoteArgs;

pub async fn register(
    _remote_args: RemoteArgs,
    _provisioning_key: String,
) -> Result<(), Infallible> {
    todo!("implement device provisioning")
}
