use std::path::PathBuf;

use ployz_core::nats_config::{
    CredentialGrant, CredentialName, CredentialRole, NatsAuthorizationGrant, NatsInternalAuthority,
    NatsUserPublicKey,
};
use ployz_core::ops::{
    DeployRunningStage, NamespaceRemoveRunningStage, NetworkRepairRunningStage,
    ServiceRestartRunningStage, VolumeRemoveRunningStage,
};
use ployz_core::security::NatsPrincipal;
use ployz_nats::endpoints::{MachineServiceEndpoint, OperationApiEndpoint};
use ployz_nats::permissions::{NatsPermissionProfile, inbox_prefix, render_authorized_users};
use ployz_nats::server_config::{
    NatsAdvertisedHost, NatsListener, NatsServerConfig, NatsServerConfigError, NatsServerTlsFiles,
};
use ployz_nats::subjects::{
    INTENT_CHANGED, OPERATION_PROGRESS_SCOPE, OperationProgressScope, deploy_running_stage,
    gateway_status, machine_facts, machine_service, namespace_remove_running_stage,
    network_repair_running_stage, operation_progress_watch, service_restart_running_stage,
    volume_remove_running_stage,
};
use ployz_test_support::ids::{machine_id, namespace_id, operation_id};

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
            authority: NatsInternalAuthority::Controller,
            public_key: user_public_key(),
        },
        NatsAuthorizationGrant::Credential(CredentialGrant {
            public_key: user_public_key(),
            name: CredentialName::try_new("Ployz Cloud").expect("valid credential name"),
            role: CredentialRole::Operator,
        }),
    ];
    let rendered = render_authorized_users(&grants);
    assert!(rendered.contains("# ployz-principal: controller"));
    assert!(rendered.contains("# ployz-credential-role: operator"));

    let principals = [
        NatsPrincipal::Machine {
            machine_id: machine_id("machine_7"),
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
fn subjects_scopes_and_endpoints_match_the_core_compatibility_surface() {
    let machine_id = machine_id("machine_7");
    let operation_id = operation_id("op_123");
    let scope = OperationProgressScope::Namespace {
        namespace_id: namespace_id("default"),
    };

    assert_eq!(INTENT_CHANGED, ployz_core::subjects::INTENT_CHANGED);
    assert_eq!(
        OPERATION_PROGRESS_SCOPE,
        ployz_core::subjects::OPERATION_PROGRESS_SCOPE
    );
    assert_eq!(
        machine_service(&machine_id, MachineServiceEndpoint::ContainerRun),
        ployz_core::subjects::machine_service(
            &machine_id,
            ployz_core::subjects::MachineServiceEndpoint::ContainerRun,
        )
    );
    assert_eq!(
        OperationApiEndpoint::DeploySubmit.subject(),
        ployz_core::subjects::OperationApiEndpoint::DeploySubmit.subject()
    );
    assert_eq!(
        operation_progress_watch(&scope, &operation_id),
        ployz_core::subjects::operation_progress_watch(
            &ployz_core::subjects::OperationProgressScope::Namespace {
                namespace_id: namespace_id("default"),
            },
            &operation_id,
        )
    );
    assert_eq!(
        machine_facts(&machine_id),
        ployz_core::subjects::machine_facts(&machine_id)
    );
    assert_eq!(
        gateway_status(&machine_id),
        ployz_core::subjects::gateway_status(&machine_id)
    );
}

#[test]
fn every_operation_endpoint_matches_the_core_compatibility_surface() {
    use ployz_core::subjects::OperationApiEndpoint as Core;
    use ployz_nats::endpoints::OperationApiEndpoint as Nats;

    let endpoints = [
        (Nats::DeployReserve, Core::DeployReserve),
        (Nats::DeploySubmit, Core::DeploySubmit),
        (
            Nats::InitFirstMachineActivate,
            Core::InitFirstMachineActivate,
        ),
        (Nats::MachineAdd, Core::MachineAdd),
        (Nats::MachineUpdate, Core::MachineUpdate),
        (Nats::MachineDrain, Core::MachineDrain),
        (Nats::MachineResume, Core::MachineResume),
        (Nats::MachineList, Core::MachineList),
        (Nats::MachineInspect, Core::MachineInspect),
        (Nats::NetworkStatus, Core::NetworkStatus),
        (Nats::NetworkResolve, Core::NetworkResolve),
        (Nats::NetworkRepair, Core::NetworkRepair),
        (Nats::MachineJoinRedeem, Core::MachineJoinRedeem),
        (Nats::MachineJoinReport, Core::MachineJoinReport),
        (Nats::ServiceList, Core::ServiceList),
        (Nats::ServiceInspect, Core::ServiceInspect),
        (Nats::ServiceRestart, Core::ServiceRestart),
        (Nats::NamespaceRemove, Core::NamespaceRemove),
        (Nats::VolumeList, Core::VolumeList),
        (Nats::VolumeRemove, Core::VolumeRemove),
        (Nats::RuntimeSnapshot, Core::RuntimeSnapshot),
        (Nats::LogsTail, Core::LogsTail),
        (Nats::OpsList, Core::OpsList),
        (Nats::OpsStatus, Core::OpsStatus),
        (Nats::OpsWatch, Core::OpsWatch),
        (Nats::CoreReplace, Core::CoreReplace),
        (Nats::CoreReplaceReport, Core::CoreReplaceReport),
        (Nats::CredentialAdd, Core::CredentialAdd),
        (Nats::CredentialList, Core::CredentialList),
        (Nats::CredentialRemove, Core::CredentialRemove),
        (Nats::IngressConfigure, Core::IngressConfigure),
    ];

    for (nats, core) in endpoints {
        assert_eq!(nats.name(), core.name());
        assert_eq!(nats.subject(), core.subject());
        assert_eq!(
            matches!(
                nats.execution(),
                ployz_nats::endpoints::OperationApiEndpointExecution::AcceptsOperation
            ),
            matches!(
                core.execution(),
                ployz_core::subjects::OperationApiEndpointExecution::AcceptsOperation
            )
        );
        assert_eq!(
            matches!(
                nats.execution(),
                ployz_nats::endpoints::OperationApiEndpointExecution::MutatesOperation
            ),
            matches!(
                core.execution(),
                ployz_core::subjects::OperationApiEndpointExecution::MutatesOperation
            )
        );
    }
}

#[test]
fn every_machine_endpoint_matches_the_core_compatibility_surface() {
    use ployz_core::subjects::MachineServiceEndpoint as Core;
    use ployz_nats::endpoints::MachineServiceEndpoint as Nats;

    let endpoints = [
        (Nats::Inspect, Core::Inspect),
        (Nats::FactsGet, Core::FactsGet),
        (Nats::FactsRefresh, Core::FactsRefresh),
        (Nats::DnsResolve, Core::DnsResolve),
        (Nats::DnsStatus, Core::DnsStatus),
        (Nats::ContainerInspect, Core::ContainerInspect),
        (Nats::ContainerResolveImage, Core::ContainerResolveImage),
        (Nats::ContainerRun, Core::ContainerRun),
        (Nats::ContainerRunHook, Core::ContainerRunHook),
        (Nats::ContainerRestart, Core::ContainerRestart),
        (Nats::ContainerStop, Core::ContainerStop),
        (Nats::ContainerRemove, Core::ContainerRemove),
        (Nats::VolumeRemove, Core::VolumeRemove),
        (Nats::DataplanePublicKey, Core::DataplanePublicKey),
        (Nats::DataplaneStatus, Core::DataplaneStatus),
        (Nats::SubstrateUpdate, Core::SubstrateUpdate),
        (Nats::SubstrateReport, Core::SubstrateReport),
        (Nats::LogsTail, Core::LogsTail),
        (Nats::ImageBlobCheck, Core::ImageBlobCheck),
        (Nats::ImageBlobPush, Core::ImageBlobPush),
        (Nats::ImageManifestPush, Core::ImageManifestPush),
        (Nats::ImageEnsure, Core::ImageEnsure),
        (Nats::CertificateArtifactPush, Core::CertificateArtifactPush),
        (
            Nats::CertificateArtifactRemove,
            Core::CertificateArtifactRemove,
        ),
        (
            Nats::CertificateChallengeApply,
            Core::CertificateChallengeApply,
        ),
        (
            Nats::CertificateChallengeRemove,
            Core::CertificateChallengeRemove,
        ),
        (
            Nats::CertificateChallengeStatus,
            Core::CertificateChallengeStatus,
        ),
        (Nats::GatewayStatusGet, Core::GatewayStatusGet),
    ];
    let machine_id = machine_id("machine_7");

    for (nats, core) in endpoints {
        assert_eq!(nats.as_subject(), core.as_subject());
        assert_eq!(
            matches!(
                nats.execution(),
                ployz_nats::endpoints::MachineServiceEndpointExecution::Query
            ),
            matches!(
                core.execution(),
                ployz_core::subjects::MachineServiceEndpointExecution::Query
            )
        );
        assert_eq!(
            machine_service(&machine_id, nats),
            ployz_core::subjects::machine_service(&machine_id, core)
        );
    }
}

#[test]
fn operation_stage_subjects_match_the_core_compatibility_surface() {
    for stage in [
        DeployRunningStage::EnsuringImages,
        DeployRunningStage::StartingContainers,
        DeployRunningStage::WaitingForHealth,
        DeployRunningStage::EnsuringCertificates,
        DeployRunningStage::RouteCutover,
        DeployRunningStage::ServingTargetCommit,
        DeployRunningStage::RemovingSupersededContainers,
    ] {
        assert_eq!(deploy_running_stage(&stage), stage.as_subject());
    }
    for stage in [
        ServiceRestartRunningStage::RestartingContainers,
        ServiceRestartRunningStage::WaitingForHealth,
    ] {
        assert_eq!(service_restart_running_stage(&stage), stage.as_subject());
    }
    for stage in [
        NetworkRepairRunningStage::AwaitingDataplane,
        NetworkRepairRunningStage::RefreshingMachineFacts,
        NetworkRepairRunningStage::ConfirmingDnsRefresh,
    ] {
        assert_eq!(network_repair_running_stage(&stage), stage.as_subject());
    }
    for stage in [
        NamespaceRemoveRunningStage::RemovingRouteBindings,
        NamespaceRemoveRunningStage::RemovingServingTargets,
        NamespaceRemoveRunningStage::RemovingContainers,
    ] {
        assert_eq!(namespace_remove_running_stage(&stage), stage.as_subject());
    }
    let stage = VolumeRemoveRunningStage::RemovingVolumeData;
    assert_eq!(volume_remove_running_stage(&stage), stage.as_subject());
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
