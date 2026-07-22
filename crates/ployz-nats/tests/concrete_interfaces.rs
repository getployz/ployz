use std::path::PathBuf;

use ployz_core::build::BUILD_RESPONSE_PERMISSION_EXPIRY;
use ployz_core::ids::{BuildExecutorId, BuildPoolId};
use ployz_core::nats_config::{
    BuildExecutorCredentialExpiresAt, CredentialGrant, CredentialName, CredentialRole,
    MintedNatsUser, NatsAuthorizationGrant, NatsInternalAuthority, NatsUserPublicKey,
};
use ployz_core::security::NatsPrincipal;
use ployz_nats::permissions::{
    AuthorizedUsersParseError, NatsPermissionProfile, inbox_prefix, parse_authorized_users,
    render_authorized_users,
};
use ployz_nats::server_config::{
    NatsAdvertisedHost, NatsListener, NatsServerConfig, NatsServerConfigError, NatsServerTlsFiles,
};
use ployz_nats::subjects::OperationApiEndpoint;
use ployz_test_support::ids::machine_id;

#[test]
fn package_typescript_contract_uses_nats_owned_endpoint_metadata() {
    assert_eq!(
        include_str!("../../../packages/ployz-sdk/src/generated.ts"),
        ployz_nats::typescript::generated_typescript()
    );
}

#[test]
fn sdk_contract_registry_maps_to_every_nats_operation_endpoint() {
    let mut endpoints = Vec::new();
    macro_rules! push_endpoints {
        ($($contract:ty),+ $(,)?) => {
            $(endpoints.push(OperationApiEndpoint::from(
                <$contract as ployz_sdk_types::operation_api::OperationApiContract>::ENDPOINT,
            ));)+
        };
    }
    ployz_sdk_types::operation_api_contracts!(push_endpoints);

    assert_eq!(
        endpoints,
        [
            OperationApiEndpoint::BuildTargetCapabilities,
            OperationApiEndpoint::BuildSubmit,
            OperationApiEndpoint::BuildCancel,
            OperationApiEndpoint::DeployReserve,
            OperationApiEndpoint::DeployPreview,
            OperationApiEndpoint::DeploySubmit,
            OperationApiEndpoint::SystemDeploy,
            OperationApiEndpoint::InitFirstMachineActivate,
            OperationApiEndpoint::MachineAdd,
            OperationApiEndpoint::MachineBuildCachePrune,
            OperationApiEndpoint::MachineUpdate,
            OperationApiEndpoint::MachineStoragePrepare,
            OperationApiEndpoint::MachineStoragePrepareCancel,
            OperationApiEndpoint::MachineDrain,
            OperationApiEndpoint::MachineResume,
            OperationApiEndpoint::ServiceRestart,
            OperationApiEndpoint::NamespaceRemove,
            OperationApiEndpoint::VolumeCreate,
            OperationApiEndpoint::VolumeRemove,
            OperationApiEndpoint::CoreReplace,
            OperationApiEndpoint::CoreReplaceReport,
            OperationApiEndpoint::CredentialAdd,
            OperationApiEndpoint::CredentialList,
            OperationApiEndpoint::CredentialRemove,
            OperationApiEndpoint::IngressConfigure,
            OperationApiEndpoint::MachineList,
            OperationApiEndpoint::MachineInspect,
            OperationApiEndpoint::NetworkStatus,
            OperationApiEndpoint::NetworkResolve,
            OperationApiEndpoint::NetworkRepair,
            OperationApiEndpoint::MachineJoinRedeem,
            OperationApiEndpoint::MachineJoinReport,
            OperationApiEndpoint::ServiceList,
            OperationApiEndpoint::VolumeList,
            OperationApiEndpoint::ServiceInspect,
            OperationApiEndpoint::RuntimeSnapshot,
            OperationApiEndpoint::LogsTail,
            OperationApiEndpoint::OpsList,
            OperationApiEndpoint::OpsStatus,
            OperationApiEndpoint::OpsWatch,
        ]
    );
}

#[test]
fn server_configuration_renders_external_and_loopback_listeners() {
    let external = external_server_config();
    assert_eq!(external.client_host(), "core.example.test");
    assert!(external.render().contains("host: 0.0.0.0\n"));
    assert!(
        external
            .render()
            .contains("client_advertise: \"core.example.test:4222\"\n")
    );

    let loopback = NatsServerConfig::single_machine(
        machine_id("core_1"),
        NatsListener::Loopback,
        tls_files(),
        PathBuf::from("authorized-users.conf"),
    )
    .expect("valid loopback NATS server config");
    assert_eq!(loopback.client_host(), "127.0.0.1");
    assert!(loopback.render().contains("host: 127.0.0.1\n"));
    assert!(loopback.render().contains("jetstream: disabled\n"));
}

#[test]
fn server_configuration_validates_advertised_hosts_and_tls_paths() {
    assert!(NatsAdvertisedHost::try_new("core.example.test").is_ok());
    assert!(NatsAdvertisedHost::try_new("203.0.113.10").is_ok());
    assert!(NatsAdvertisedHost::try_new("[2001:db8::1]").is_ok());
    assert!(NatsAdvertisedHost::try_new("under_score").is_err());

    assert_eq!(
        NatsServerConfig::single_machine(
            machine_id("core_1"),
            NatsListener::Loopback,
            NatsServerTlsFiles {
                cert_file: PathBuf::from("relative/server.crt"),
                key_file: PathBuf::from("/var/lib/ployz/nats/server.key"),
            },
            PathBuf::from("authorized-users.conf"),
        ),
        Err(NatsServerConfigError::InvalidPath {
            field: "tls.cert_file",
            value: PathBuf::from("relative/server.crt"),
        })
    );
}

#[test]
fn authorization_and_permission_rendering_are_owned_here() {
    let grants = [
        NatsAuthorizationGrant::Internal {
            authority: NatsInternalAuthority::Machine {
                machine_id: machine_id("machine_7"),
            },
            public_key: user_public_key(),
        },
        NatsAuthorizationGrant::Internal {
            authority: NatsInternalAuthority::Controller,
            public_key: user_public_key(),
        },
        NatsAuthorizationGrant::Credential(CredentialGrant {
            public_key: user_public_key(),
            name: CredentialName::try_new("Ployz Cloud").expect("valid credential name"),
            role: CredentialRole::Operator,
        }),
        NatsAuthorizationGrant::Credential(CredentialGrant {
            public_key: another_user_public_key(),
            name: CredentialName::try_new("external builder").expect("credential name"),
            role: CredentialRole::BuildExecutor {
                pool_id: BuildPoolId::try_new("pool-west").expect("pool"),
                executor_id: BuildExecutorId::try_new("builder_1").expect("executor"),
                expires_at: BuildExecutorCredentialExpiresAt::try_new(2_000_000_000)
                    .expect("expiry"),
            },
        }),
    ];
    let rendered = render_authorized_users(&grants);
    assert!(rendered.contains("# ployz-principal: controller"));
    assert!(rendered.contains("# ployz-credential-role: operator"));
    assert!(
        rendered.contains("# ployz-credential-role: build_executor.pool-west.builder_1.2000000000")
    );
    assert!(rendered.contains(&format!(
        "allow_responses: {{ max: 1, expires: {}s }}",
        BUILD_RESPONSE_PERMISSION_EXPIRY.as_secs()
    )));
    assert!(rendered.contains("allow_responses: { max: 1, expires: 120s }"));
    assert!(!rendered.contains("allow_responses: true"));
    assert_eq!(
        parse_authorized_users(&rendered).expect("rendered authorizations parse"),
        grants
    );

    let principals = [
        NatsPrincipal::Machine {
            machine_id: machine_id("machine_7"),
        },
        NatsPrincipal::BuildExecutor {
            pool_id: BuildPoolId::try_new("pool-west").expect("pool"),
            executor_id: BuildExecutorId::try_new("builder_1").expect("executor"),
        },
        NatsPrincipal::Controller,
        NatsPrincipal::Operator,
        NatsPrincipal::Join,
        NatsPrincipal::System,
    ];
    for principal in principals {
        let nats = NatsPermissionProfile::render(principal.clone());
        assert_eq!(nats.principal, principal);
        assert!(
            nats.subscribe
                .allowed_subjects()
                .contains(&format!("{}.>", inbox_prefix(&principal)))
        );
    }
}

#[test]
fn authorization_parser_rejects_a_role_that_disagrees_with_its_principal_marker() {
    let grant = NatsAuthorizationGrant::Credential(CredentialGrant {
        public_key: another_user_public_key(),
        name: CredentialName::try_new("external builder").expect("credential name"),
        role: CredentialRole::BuildExecutor {
            pool_id: BuildPoolId::try_new("pool-west").expect("pool"),
            executor_id: BuildExecutorId::try_new("builder_1").expect("executor"),
            expires_at: BuildExecutorCredentialExpiresAt::try_new(2_000_000_000).expect("expiry"),
        },
    });
    let rendered = render_authorized_users(&[grant]).replace(
        "# ployz-principal: build_executor.pool-west.builder_1",
        "# ployz-principal: build_executor.pool-west.builder_2",
    );

    assert!(matches!(
        parse_authorized_users(&rendered),
        Err(AuthorizedUsersParseError::PrincipalCredentialMismatch { .. })
    ));
}

fn external_server_config() -> NatsServerConfig {
    NatsServerConfig::single_machine(
        machine_id("core_1"),
        NatsListener::External {
            advertise_host: NatsAdvertisedHost::try_new("core.example.test")
                .expect("valid advertised host"),
        },
        tls_files(),
        PathBuf::from("authorized-users.conf"),
    )
    .expect("valid NATS server config")
}

fn tls_files() -> NatsServerTlsFiles {
    NatsServerTlsFiles {
        cert_file: PathBuf::from("/var/lib/ployz/nats/server.crt"),
        key_file: PathBuf::from("/var/lib/ployz/nats/server.key"),
    }
}

fn user_public_key() -> NatsUserPublicKey {
    NatsUserPublicKey::try_new("UBCXCMGAZQZN55X5TTTWMB5CZNZIKJHEDZJOJ3TV63NKPJ6FRXSR2ZO4")
        .expect("valid user public key")
}

fn another_user_public_key() -> NatsUserPublicKey {
    MintedNatsUser::generate().expect("user mints").public
}
