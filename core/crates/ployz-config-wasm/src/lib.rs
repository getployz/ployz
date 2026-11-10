//! The SDK's WASM ABI for Node and the browser: config rules, the deploy approval a
//! browser key signs, the automatic deploy rule a passkey answers, and the review of a
//! preview a person signs. All policy lives in ployz-core.
use ployz_core::authority::{
    AutoDeployRule, DeployApproval, DeployTerms, PreviewReview, PreviewRule, ReviewTerms,
    RuleBasis, RuleTerms, Signed, SignerKey, Timestamp,
};
use ployz_core::{Namespace, RequestedServiceSpec, ServiceName};
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
/// as [`AutoDeployRule::draft`] builds it: its canonical body and what it shows. The
/// browser takes each Service's digest with the code the CLI and the Servers run.
///
/// # Errors
/// Rejects malformed input and whatever `draft` refuses.
#[wasm_bindgen]
pub fn auto_deploy_rule_body(input: &str) -> Result<String, JsError> {
    let input: RuleInput =
        serde_json::from_str(input).map_err(|error| JsError::new(&error.to_string()))?;
    let services = input
        .services
        .unwrap_or_else(|| input.view.branch_services());
    let draft = AutoDeployRule::draft(&input.view, &services, input.terms)
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

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ReviewInput {
    rule: Signed<PreviewRule>,
    namespace: Namespace,
    target: Vec<RequestedServiceSpec>,
    terms: ReviewTerms,
}

/// The review a person signs of a preview's `target` in `namespace` under `rule` and
/// `terms`, as [`PreviewReview::draft`] builds it, so the browser hashes each spec with
/// the code the Servers run. The rule's signature isn't checked here: every Server does.
///
/// # Errors
/// Rejects malformed input, an unreadable rule, and whatever `draft` refuses.
#[wasm_bindgen]
pub fn preview_review_body(input: &str) -> Result<String, JsError> {
    let input: ReviewInput =
        serde_json::from_str(input).map_err(|error| JsError::new(&error.to_string()))?;
    let rule = input
        .rule
        .open()
        .map_err(|_| JsError::new("The preview rule isn't readable. Approve it again."))?;
    let draft = PreviewReview::draft(&rule, &input.namespace, &input.target, input.terms)
        .map_err(|error| JsError::new(&error.to_string()))?;
    serde_json::to_string(&draft).map_err(|error| JsError::new(&error.to_string()))
}
