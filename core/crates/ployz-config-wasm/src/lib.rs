//! The SDK's WASM ABI for Node and the browser: config rules, the preview rule and turning
//! an automatic deploy rule off, a commit approved for one preview, Private Access approval, join and versions a passkey answers, and the key an install line
//! carries.
//! All policy lives in ployz-core.
use ployz_core::OperationId;
use ployz_core::authority::{
    AutoDeployRule, CommitApproval, JoinApproval, Passkey, PreviewBasis, PrivateAccessApproval,
    PrivateAccessTerms, RuleBasis, RuleTerms, SignerKey, Timestamp, TrashApproval, TrashTerms,
    version_draft, version_draft::VersionsAsked,
};
use ployz_core::config::{ConfigError, Generator, parse_env_file, parse_service_setting};
use ployz_core::first_key::LineKey;
use ployz_core::{JoinedServer, MachineSubnet, Region};
use wasm_bindgen::prelude::*;

#[derive(serde::Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum ConfigRequest {
    ParseSetting { value: serde_json::Value },
    ParseEnvFile { text: String },
    ParseRegion { value: String },
    ParseRegionName { value: String },
    FirstServerRegion { servers: Vec<ServerRegion> },
    ParseGenerator { value: String },
}

/// A Server's Machine Subnet, when its join was approved and its `region` label value, as a
/// caller sees them.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ServerRegion {
    subnet: Option<MachineSubnet>,
    #[serde(default)]
    joined: Option<Timestamp>,
    region: Option<String>,
}

/// Decode a JSON request and invoke the pure configuration API in ployz-core; it validates
/// before policy.
///
/// # Errors
/// Rejects malformed requests, invalid configuration, and unserializable results. Messages
/// identify the rejected setting without echoing its value.
#[wasm_bindgen]
pub fn config_request(input: &str) -> Result<String, JsError> {
    let input: serde_json::Value =
        serde_json::from_str(input).map_err(|_| JsError::new("Invalid configuration request"))?;
    answer(input)
        .and_then(|output| {
            serde_json::to_string(&output)
                .map_err(|_| refused("result", "Invalid configuration result"))
        })
        .map_err(|error| JsError::new(&error.to_string()))
}

fn answer(input: serde_json::Value) -> Result<serde_json::Value, ConfigError> {
    let input = serde_json::from_value(input)
        .map_err(|_| refused("request", "Invalid configuration request"))?;
    match input {
        ConfigRequest::ParseSetting { value } => parse_service_setting(value),
        ConfigRequest::ParseEnvFile { text } => {
            Ok(serde_json::to_value(parse_env_file(&text)?).expect("env entries are JSON"))
        }
        ConfigRequest::ParseRegion { value } => Region::parse(value)
            .map(|region| serde_json::Value::String(region.into()))
            .map_err(|_| {
                refused(
                    "region",
                    "A region is up to 63 letters, digits, hyphens, underscores and dots.",
                )
            }),
        ConfigRequest::ParseRegionName { value } => Region::display_name(&value)
            .map(|name| serde_json::Value::String(name.to_owned()))
            .ok_or_else(|| refused("name", Region::NAME_RULE)),
        ConfigRequest::FirstServerRegion { servers } => Ok(serde_json::to_value(
            Region::of_first_server(servers.iter().map(|server| match server.subnet {
                Some(subnet) => JoinedServer::Read {
                    subnet,
                    joined: server.joined,
                    label: server.region.as_deref(),
                },
                None => JoinedServer::Unread,
            })),
        )
        .expect("a region is JSON")),
        ConfigRequest::ParseGenerator { value } => match Generator::parse(&value) {
            Ok(Some(generator)) => Ok(serde_json::json!({
                "length": generator.length(),
                "alphabet": String::from_utf8_lossy(&generator.alphabet()),
            })),
            Ok(None) => Err(refused("generated", "Expected a secret(…) call.")),
            Err(error) => Err(refused("generated", &error.to_string())),
        },
    }
}

fn refused(path: &str, message: &str) -> ConfigError {
    ConfigError {
        path: path.into(),
        message: message.into(),
    }
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
struct PreviewInput {
    view: PreviewBasis,
    terms: RuleTerms,
}

/// The preview rule a passkey answers for the Store's preview rule `view`, as
/// [`AutoDeployRule::draft_preview`] drafts it: its canonical body and what the person reads.
///
/// # Errors
/// Rejects malformed input, and a view with a Service no preview rule can cover.
#[wasm_bindgen]
pub fn preview_rule_body(input: &str) -> Result<String, JsError> {
    let input: PreviewInput =
        serde_json::from_str(input).map_err(|error| JsError::new(&error.to_string()))?;
    let draft = AutoDeployRule::draft_preview(&input.view, input.terms)
        .map_err(|error| JsError::new(&error.to_string()))?
        .drafted_preview(&input.view)
        .map_err(|error| JsError::new(&error.to_string()))?;
    serde_json::to_string(&draft).map_err(|error| JsError::new(&error.to_string()))
}

/// The approval of one commit for one preview a passkey answers, in canonical form, as
/// every Server reads it.
///
/// # Errors
/// Rejects malformed input, such as a commit or Namespace that isn't one.
#[wasm_bindgen]
pub fn commit_approval_body(input: &str) -> Result<String, JsError> {
    let approval: CommitApproval =
        serde_json::from_str(input).map_err(|error| JsError::new(&error.to_string()))?;
    ployz_core::authority::canonical::to_canonical(&approval)
        .map_err(|error| JsError::new(&error.to_string()))
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

/// The Trash approval a passkey answers for `terms`, in canonical form, its entries sorted
/// and each once, by the ployz-core code the Servers run.
///
/// # Errors
/// Rejects malformed terms.
#[wasm_bindgen]
pub fn trash_approval_body(input: &str) -> Result<String, JsError> {
    let terms: TrashTerms =
        serde_json::from_str(input).map_err(|error| JsError::new(&error.to_string()))?;
    ployz_core::authority::canonical::to_canonical(&TrashApproval::drafted(&terms))
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

/// One version of each Service `input` asks for, and the Source step it asks for, as steps
/// of one bundle a passkey answers, as [`VersionsAsked::draft`] builds it: the bundle, each
/// step body with what the Store records beside it, and the lines the person reads. `null`
/// when only a Source step was asked for and the kept rule needs none.
///
/// # Errors
/// Rejects malformed input and whatever `draft` refuses, before any bundle.
#[wasm_bindgen]
pub fn version_steps(input: &str) -> Result<String, JsError> {
    let input: VersionsAsked =
        serde_json::from_str(input).map_err(|error| JsError::new(&error.to_string()))?;
    let draft = input
        .draft()
        .map_err(|error| JsError::new(&error.to_string()))?;
    serde_json::to_string(&draft).map_err(|error| JsError::new(&error.to_string()))
}

/// What a Save of the Store's staged rule `view` needs for its Source step, as
/// [`version_draft::source_need`] reads it from the view alone: `"none"`, `"servers"` when
/// only the Servers the kept rule names may be out of date, or `"rule"`.
///
/// # Errors
/// Rejects a malformed view.
#[wasm_bindgen]
pub fn source_need(view: &str) -> Result<String, JsError> {
    let view: RuleBasis =
        serde_json::from_str(view).map_err(|error| JsError::new(&error.to_string()))?;
    serde_json::to_string(&version_draft::source_need(&view))
        .map_err(|error| JsError::new(&error.to_string()))
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
