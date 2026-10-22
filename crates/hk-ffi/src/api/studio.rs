//! Studio JSON boundary: the same operations are served by a remote daemon.
use crate::error::HkError;

pub async fn studio_request(operation: String, args_json: String) -> Result<String, HkError> {
    crate::handle::core()?
        .studio_request(operation, args_json)
        .await
}
