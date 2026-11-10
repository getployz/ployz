//! The SDK's WASM ABI for Node and the browser: config rules and the deploy approval a
//! browser key signs. All policy lives in ployz-core.
use ployz_core::authority::{DeployApproval, DeployTerms};
use ployz_core::{Namespace, RequestedServiceSpec};
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

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DraftInput {
    namespace: Namespace,
    target: Vec<RequestedServiceSpec>,
    terms: DeployTerms,
}

/// The deploy approval a browser key signs for `target` in `namespace` under `terms`, as
/// [`DeployApproval::browser_draft`] builds it, so the browser hashes each spec with the
/// code the CLI and the Servers run.
///
/// # Errors
/// Rejects malformed input and whatever `browser_draft` refuses.
#[wasm_bindgen]
pub fn deploy_approval_body(input: &str) -> Result<String, JsError> {
    let input: DraftInput =
        serde_json::from_str(input).map_err(|error| JsError::new(&error.to_string()))?;
    let draft = DeployApproval::browser_draft(input.terms, input.namespace, &input.target)
        .map_err(|error| JsError::new(&error.to_string()))?;
    serde_json::to_string(&draft).map_err(|error| JsError::new(&error.to_string()))
}
