//! The SDK's WASM ABI for Node and the browser: config rules, the automatic deploy rule,
//! Private Access approval and join a passkey answers, and the key an install line carries.
//! All policy lives in ployz-core.
use ployz_core::OperationId;
use ployz_core::ServiceName;
use ployz_core::authority::{
    AutoDeployRule, JoinApproval, Passkey, PrivateAccessApproval, PrivateAccessTerms, RuleBasis,
    RuleTerms, SignerKey, Timestamp,
};
use ployz_core::first_key::LineKey;
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
struct RuleInput {
    view: RuleBasis,
    terms: RuleTerms,
    /// The Services to approve; every one on the rule's branch when absent.
    #[serde(default)]
    services: Option<Vec<ServiceName>>,
}

/// The automatic deploy rule a person approves for the Store's rule `view` under `terms`,
/// as [`AutoDeployRule::draft`] builds it: its canonical body, what it shows and the births it mints. The
/// browser takes each Service's digest with the code the CLI and the Servers run.
///
/// # Errors
/// Rejects malformed input and whatever `draft` refuses.
#[wasm_bindgen]
pub fn auto_deploy_rule_body(input: &str) -> Result<String, JsError> {
    let mut input: RuleInput =
        serde_json::from_str(input).map_err(|error| JsError::new(&error.to_string()))?;
    let services = input
        .services
        .take()
        .unwrap_or_else(|| input.view.branch_services());
    let draft = AutoDeployRule::draft(&mut input.view, &services, input.terms)
        .map_err(|error| JsError::new(&error.to_string()))?
        .drafted(&input.view)
        .map_err(|error| JsError::new(&error.to_string()))?;
    serde_json::to_string(&draft).map_err(|error| JsError::new(&error.to_string()))
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct EndInput {
    view: RuleBasis,
    issued_at: Timestamp,
    signer: SignerKey,
}

/// The generation that turns the rule kept in `view` off, as [`AutoDeployRule::ending`]
/// builds it.
///
/// # Errors
/// Rejects malformed input, and a view whose rule is missing or already off.
#[wasm_bindgen]
pub fn auto_deploy_rule_end_body(input: &str) -> Result<String, JsError> {
    let input: EndInput =
        serde_json::from_str(input).map_err(|error| JsError::new(&error.to_string()))?;
    let draft = AutoDeployRule::ending(&input.view, input.issued_at, input.signer)
        .map_err(|error| JsError::new(&error.to_string()))?
        .drafted(&input.view)
        .map_err(|error| JsError::new(&error.to_string()))?;
    serde_json::to_string(&draft).map_err(|error| JsError::new(&error.to_string()))
}

/// The Private Access approval a passkey answers for `terms`, in canonical form, with the
/// digest of the sealed key taken here, by the ployz-core code the Servers run.
///
/// # Errors
/// Rejects malformed terms.
#[wasm_bindgen]
pub fn private_access_approval_body(input: &str) -> Result<String, JsError> {
    let terms: PrivateAccessTerms =
        serde_json::from_str(input).map_err(|error| JsError::new(&error.to_string()))?;
    ployz_core::authority::canonical::to_canonical(&PrivateAccessApproval::drafted(&terms))
        .map_err(|error| JsError::new(&error.to_string()))
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct JoinInput {
    join: JoinApproval,
    operation: OperationId,
    issued_at: Timestamp,
    expires_at: Timestamp,
}

/// The waiting `join` as step 0 of a bundle a passkey answers, as
/// [`JoinApproval::passkey_step`] builds it: the step body and the bundle, each canonical.
///
/// # Errors
/// Rejects malformed input.
#[wasm_bindgen]
pub fn passkey_join_step(input: &str) -> Result<String, JsError> {
    let input: JoinInput =
        serde_json::from_str(input).map_err(|error| JsError::new(&error.to_string()))?;
    let draft = input
        .join
        .passkey_step(input.operation, input.issued_at, input.expires_at)
        .map_err(|error| JsError::new(&error.to_string()))?;
    serde_json::to_string(&draft).map_err(|error| JsError::new(&error.to_string()))
}

/// The key an install line carries for `passkey`, as `ployz key add` and the installer read
/// it: the passkey a fresh Server trusts to found or join its Cluster.
///
/// # Errors
/// Rejects a malformed passkey.
#[wasm_bindgen]
pub fn install_key(passkey: &str) -> Result<String, JsError> {
    let passkey: Passkey =
        serde_json::from_str(passkey).map_err(|error| JsError::new(&error.to_string()))?;
    Ok(LineKey::Passkey { passkey }.to_string())
}
