use anyhow::Result;

use super::cli::RemoteArgs;

pub async fn register(_remote_args: RemoteArgs, _provisioning_key: String) -> Result<()> {
    todo!("implement device provisioning")
}
