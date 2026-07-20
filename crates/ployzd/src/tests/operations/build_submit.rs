use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_util::StreamExt;
use ployz_core::build::{
    BuildAdapter, BuildCacheScope, BuildExecutorCapability, BuildExecutorIdentity,
    BuildExecutorReadiness, BuildExecutorReadinessAnswer, BuildPlatforms, BuildSource, BuildTarget,
    LocalSnapshotDigest,
};
use ployz_core::ids::{BuildExecutorId, BuildPoolId};
use ployz_core::image::OciPlatform;
use ployz_core::nats_config::{
    BuildExecutorCredentialExpiresAt, CredentialGrant, CredentialName, CredentialRole,
    MintedNatsUser,
};
use ployz_core::security::NatsPrincipal;
use ployz_nats::connect::{NatsConnectConfig, connect_authenticated};
use ployz_nats::operation_api_client::OperationApiClientError;
use ployz_nats::subjects::{BuildExecutorServiceEndpoint, build_executor_service};
use ployz_sdk_types::{
    BuildSubmitError, BuildSubmitRequest, CredentialAddRequest, OpsStatusError, OpsStatusRequest,
};
use ployz_test_support::ids::operation_id;
use ployz_test_support::ops::wait_for_terminal_status;

use crate::tests::support::control::TestNats;

#[tokio::test]
async fn external_build_without_an_image_seed_has_no_admission_side_effects() {
    let nats = TestNats::start().await;
    let config = nats.control_config();
    let runtime = nats.start_control(&config).await;
    let api = nats.api();
    let pool_id = BuildPoolId::try_new("pool_no_seed").expect("pool id");
    let executor_id = BuildExecutorId::try_new("executor_no_seed").expect("executor id");
    let minted = MintedNatsUser::generate().expect("executor credential mints");
    let credential_operation_id = operation_id("op_add_executor_no_seed");
    api.credential_add(&CredentialAddRequest {
        operation_id: credential_operation_id.clone(),
        grant: executor_credential(&minted, &pool_id, &executor_id),
    })
    .await
    .expect("credential add accepts");
    wait_for_terminal_status(&api, &credential_operation_id, Duration::from_secs(10)).await;

    let executor = connect_authenticated(
        &executor_connect_config(&nats, minted, &pool_id, &executor_id),
        Duration::from_secs(2),
    )
    .await
    .expect("executor connects");
    let readiness_subject = build_executor_service(
        &pool_id,
        &executor_id,
        BuildExecutorServiceEndpoint::ReadinessGet,
    );
    let build_start_subject = build_executor_service(
        &pool_id,
        &executor_id,
        BuildExecutorServiceEndpoint::BuildStart,
    );
    let mut readiness_requests = executor
        .subscribe(readiness_subject)
        .await
        .expect("readiness endpoint subscribes");
    let mut build_start_requests = executor
        .subscribe(build_start_subject)
        .await
        .expect("build endpoint subscribes");
    executor.flush().await.expect("subscriptions flush");

    let responder = executor.clone();
    let identity = BuildExecutorIdentity {
        pool_id: pool_id.clone(),
        executor_id: executor_id.clone(),
    };
    let platform = OciPlatform::try_new("linux", "amd64").expect("platform");
    let readiness_platform = platform.clone();
    let readiness_task = tokio::spawn(async move {
        let request = readiness_requests
            .next()
            .await
            .expect("readiness request arrives");
        let reply = request.reply.expect("readiness request has reply subject");
        let payload = serde_json::to_vec(&BuildExecutorReadinessAnswer {
            identity,
            readiness: BuildExecutorReadiness {
                native_platform: readiness_platform,
                capability: BuildExecutorCapability::DockerfileAndRailpack,
            },
        })
        .expect("readiness serializes");
        responder
            .publish(reply, payload.into())
            .await
            .expect("readiness replies");
    });

    let build_operation_id = operation_id("build_no_image_seed");
    let error = api
        .build_submit(&BuildSubmitRequest {
            operation_id: build_operation_id.clone(),
            target: BuildTarget::External {
                pool_id: pool_id.clone(),
            },
            source: BuildSource::LocalSnapshot {
                digest: LocalSnapshotDigest::try_new(format!("sha256:{}", "a".repeat(64)))
                    .expect("snapshot digest"),
                subdir: None,
            },
            adapter: BuildAdapter::Railpack {
                cache_scope: BuildCacheScope::try_new("no-seed").expect("cache scope"),
            },
            platforms: BuildPlatforms::try_new([platform]).expect("platforms"),
        })
        .await
        .expect_err("missing image seed rejects build");
    readiness_task.await.expect("readiness responder completes");

    assert!(matches!(
        error,
        OperationApiClientError::Domain {
            error: BuildSubmitError::NoReachableImageSeed {
                operation_id,
                pool_id: rejected_pool,
            },
            ..
        } if operation_id == build_operation_id && rejected_pool == pool_id
    ));
    assert!(matches!(
        api.ops_status(&OpsStatusRequest {
            operation_id: build_operation_id.clone(),
        })
        .await,
        Err(OperationApiClientError::Domain {
            error: OpsStatusError::NoSuchOperation { operation_id },
            ..
        }) if operation_id == build_operation_id
    ));
    assert!(
        tokio::time::timeout(Duration::from_millis(250), build_start_requests.next())
            .await
            .is_err(),
        "rejected build must not start an executor"
    );

    runtime.shutdown().await.expect("runtime shuts down");
}

fn executor_credential(
    minted: &MintedNatsUser,
    pool_id: &BuildPoolId,
    executor_id: &BuildExecutorId,
) -> CredentialGrant {
    CredentialGrant {
        public_key: minted.public.clone(),
        name: CredentialName::try_new("No-seed executor").expect("credential name"),
        role: CredentialRole::BuildExecutor {
            pool_id: pool_id.clone(),
            executor_id: executor_id.clone(),
            expires_at: BuildExecutorCredentialExpiresAt::try_new(current_unix_seconds() + 300)
                .expect("expiry"),
        },
    }
}

fn executor_connect_config(
    nats: &TestNats,
    minted: MintedNatsUser,
    pool_id: &BuildPoolId,
    executor_id: &BuildExecutorId,
) -> NatsConnectConfig {
    nats.server().config_with_seed(
        NatsPrincipal::BuildExecutor {
            pool_id: pool_id.clone(),
            executor_id: executor_id.clone(),
        },
        minted.seed,
    )
}

fn current_unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock follows Unix epoch")
        .as_secs()
}
