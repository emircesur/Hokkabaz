//! Instances: this device plus remote `hokkabazd`s. Tokens go in once (`add_instance`,
//! `pair_from_uri`) and are never returned: `InstanceDto` carries only `has_token`.

use crate::dto::{InstanceDto, InstanceStatusDto};
use crate::error::HkError;
use crate::instances::manager;

/// `This device (local)` first, then remotes in the order added.
pub async fn list_instances() -> Result<Vec<InstanceDto>, HkError> {
    Ok(manager()?.list())
}

/// Store a remote by URL + API token. Does not connect; see `connect_instance`.
pub async fn add_instance(
    name: String,
    base_url: String,
    token: String,
) -> Result<InstanceDto, HkError> {
    manager()?.add(&name, &base_url, &token)
}

/// Add a remote from a pairing link (`hokkabaz://pair?url=..&token=..`, what the QR carries):
/// the one-time token is exchanged for the instance's API token. `name` defaults to host:port.
pub async fn pair_from_uri(uri: String, name: Option<String>) -> Result<InstanceDto, HkError> {
    manager()?.pair(&uri, name.as_deref()).await
}

/// Forget a remote and its token. Removing the connected one switches back to this device.
pub async fn remove_instance(id: String) -> Result<(), HkError> {
    manager()?.remove(&id).await
}

/// Instance to connect at the next app start (`local` for this device). Does not switch now.
pub async fn select_instance(id: String) -> Result<InstanceDto, HkError> {
    manager()?.select(&id)
}

/// Switch the active core. Every `chat::*` / `settings::*` call afterwards goes to `id`; streams
/// of the previous remote end, streams of the local core keep running (drop them in Dart).
/// A remote that fails its health/auth probe is not switched to.
pub async fn connect_instance(id: String) -> Result<InstanceDto, HkError> {
    manager()?.connect(&id).await
}

/// Reachability, auth and daemon version of `id`.
pub async fn instance_status(id: String) -> Result<InstanceStatusDto, HkError> {
    manager()?.status(&id).await
}
