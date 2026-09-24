//! The SDK's only config ABI, for Node and the browser. All policy lives in ployz-core.
use wasm_bindgen::prelude::*;

/// Decode a JSON request and invoke the pure configuration API in ployz-core.
///
/// # Errors
/// Rejects malformed requests, invalid configuration, and unserializable results.
#[wasm_bindgen]
pub fn config_request(input: &str) -> Result<String, JsError> {
    let input =
        serde_json::from_str(input).map_err(|_| JsError::new("Invalid configuration request"))?;
    ployz_core::config::config_request(input)
        .and_then(|output| {
            serde_json::to_string(&output).map_err(|_| ployz_core::config::ConfigError {
                path: "result".into(),
                message: "Invalid configuration result".into(),
            })
        })
        .map_err(|error| JsError::new(&error.to_string()))
}
