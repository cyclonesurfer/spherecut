//! The tools' operations (a1-plan section 6): one request type and one
//! response type each, the same for `spherecut mcp` and `spherecut call`.

pub mod doctor;

use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::error::{ToolError, codes};

/// `args` as the tool's request type. The schema has already passed
/// them, so this fails only where the schema can't say (a bug, which the
/// schema tests look for).
pub(crate) fn request<T: DeserializeOwned>(args: Value) -> Result<T, ToolError> {
    serde_json::from_value(args)
        .map_err(|e| ToolError::new(codes::BAD_ARGUMENT, format!("the arguments don't fit: {e}")))
}
