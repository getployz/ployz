use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::core_types::*;
use crate::ops::{AcceptedOperation, OperationApiResponse};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(
    type = "{ operation_id: OperationId, target?: BuildTarget, source: BuildSource, adapter: BuildAdapter, platforms: BuildPlatforms }"
)]
#[serde(deny_unknown_fields)]
pub struct BuildSubmitRequest {
    pub operation_id: OperationId,
    #[serde(default = "cluster_build_target")]
    pub target: BuildTarget,
    pub source: BuildSource,
    pub adapter: BuildAdapter,
    pub platforms: BuildPlatforms,
}

fn cluster_build_target() -> BuildTarget {
    BuildTarget::Cluster
}

pub type BuildSubmitResponse = OperationApiResponse<AcceptedOperation, BuildSubmitError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, thiserror::Error)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum BuildSubmitError {
    #[error("local snapshot source requires an external Build Target")]
    LocalSnapshotRequiresExternalTarget { operation_id: OperationId },
    #[error("operation {} already exists with a different build request", .operation_id.as_str())]
    OperationConflict { operation_id: OperationId },
    #[error("build pool {} has no capable enrolled executor for {platform:?}", .pool_id.as_str())]
    NoCapableExternalExecutor {
        operation_id: OperationId,
        pool_id: BuildPoolId,
        platform: OciPlatform,
    },
    #[error("build pool {} has no reachable active Machine image seed", .pool_id.as_str())]
    NoReachableImageSeed {
        operation_id: OperationId,
        pool_id: BuildPoolId,
    },
    #[error("build submit {} unavailable: {message}", .operation_id.as_str())]
    Unavailable {
        operation_id: OperationId,
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct BuildCancelRequest {
    pub operation_id: OperationId,
    pub reason: CancellationReason,
}

pub type BuildCancelResponse = OperationApiResponse<AcceptedOperation, BuildCancelError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, thiserror::Error)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum BuildCancelError {
    #[error("no such build operation {}", .operation_id.as_str())]
    NoSuchOperation { operation_id: OperationId },
    #[error("build operation {} is already terminal", .operation_id.as_str())]
    AlreadyTerminal { operation_id: OperationId },
    #[error("build cancellation {} unavailable: {message}", .operation_id.as_str())]
    Unavailable {
        operation_id: OperationId,
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_json() -> serde_json::Value {
        serde_json::json!({"operation_id":"build-1","source":{"source":"git","url":"https://example.com/repo.git","commit":"0123456789abcdef0123456789abcdef01234567","credential":{"username":"git","secret":"token"}},"adapter":{"adapter":"railpack","cache_scope":"scope_01J0Y1J7YK7M7SXW3NQW78J4D2"},"platforms":[{"os":"linux","architecture":"amd64"}]})
    }

    #[test]
    fn build_submit_wire_rejects_unknown_and_invalid_shapes() {
        let mut unknown = valid_json();
        unknown
            .as_object_mut()
            .expect("object")
            .insert("command".into(), serde_json::json!("unsafe"));
        assert!(serde_json::from_value::<BuildSubmitRequest>(unknown).is_err());
        let mut unknown_source = valid_json();
        unknown_source
            .as_object_mut()
            .and_then(|request| request.get_mut("source"))
            .and_then(serde_json::Value::as_object_mut)
            .expect("source object")
            .insert("branch".into(), serde_json::json!("main"));
        assert!(serde_json::from_value::<BuildSubmitRequest>(unknown_source).is_err());
        let mut duplicate = valid_json();
        duplicate
            .as_object_mut()
            .expect("request object")
            .insert("platforms".into(), serde_json::json!([{"os":"linux","architecture":"amd64"},{"os":"linux","architecture":"x86_64"}]));
        assert!(serde_json::from_value::<BuildSubmitRequest>(duplicate).is_err());
        let mut fragment = valid_json();
        fragment
            .as_object_mut()
            .and_then(|request| request.get_mut("source"))
            .and_then(serde_json::Value::as_object_mut)
            .expect("source object")
            .insert(
                "url".into(),
                serde_json::json!("https://example.com/repo#main"),
            );
        assert!(serde_json::from_value::<BuildSubmitRequest>(fragment).is_err());
    }

    #[test]
    fn durable_source_evidence_cannot_serialize_request_credential() {
        let request: BuildSubmitRequest = serde_json::from_value(valid_json()).expect("request");
        assert_eq!(request.target, BuildTarget::Cluster);
        let evidence = serde_json::to_value(request.source.evidence()).expect("evidence");
        assert!(!evidence.to_string().contains("token"));
        assert_eq!(
            evidence,
            serde_json::json!({
                "source": "git",
                "url": "https://example.com/repo.git",
                "commit": "0123456789abcdef0123456789abcdef01234567",
            })
        );
    }

    #[test]
    fn local_snapshot_wire_is_tagged_strict_and_credential_free() {
        let mut local = valid_json();
        local.as_object_mut().expect("request object").insert(
            "source".into(),
            serde_json::json!({
                "source": "local_snapshot",
                "digest": format!("sha256:{}", "a".repeat(64)),
                "subdir": "apps/api"
            }),
        );
        let request: BuildSubmitRequest =
            serde_json::from_value(local.clone()).expect("local snapshot request");
        assert!(matches!(request.source, BuildSource::LocalSnapshot { .. }));
        let evidence = serde_json::to_value(request.source.evidence()).expect("evidence");
        assert_eq!(&evidence, local.get("source").expect("source"));

        local
            .get_mut("source")
            .and_then(serde_json::Value::as_object_mut)
            .expect("source object")
            .insert(
                "credential".into(),
                serde_json::json!({"secret": "must-reject"}),
            );
        assert!(serde_json::from_value::<BuildSubmitRequest>(local).is_err());
    }

    #[test]
    fn build_submit_target_defaults_to_cluster_and_accepts_external() {
        let omitted: BuildSubmitRequest = serde_json::from_value(valid_json()).expect("request");
        assert_eq!(omitted.target, BuildTarget::Cluster);

        let mut external = valid_json();
        external.as_object_mut().expect("request object").insert(
            "target".into(),
            serde_json::json!({"target":"external","pool_id":"pool-a"}),
        );
        let external: BuildSubmitRequest = serde_json::from_value(external).expect("request");
        assert_eq!(
            external.target,
            BuildTarget::External {
                pool_id: BuildPoolId::try_new("pool-a").expect("pool id")
            }
        );
    }

    #[test]
    fn build_submit_typescript_keeps_the_defaulted_target_optional() {
        let declaration = <BuildSubmitRequest as TS>::decl(&ts_rs::Config::default());
        assert_eq!(
            declaration,
            "type BuildSubmitRequest = { operation_id: OperationId, target?: BuildTarget, source: BuildSource, adapter: BuildAdapter, platforms: BuildPlatforms };"
        );
    }
}
