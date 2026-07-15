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
    NatsAdvertisedHost, NatsListener, NatsServerConfig, NatsServerTlsFiles,
};
use ployz_nats::subjects::{
    INTENT_CHANGED, OPERATION_PROGRESS_SCOPE, OperationProgressScope, deploy_running_stage,
    gateway_status, machine_facts, machine_service, namespace_remove_running_stage,
    network_repair_running_stage, operation_progress_watch, service_restart_running_stage,
    volume_remove_running_stage,
};
use ployz_test_support::ids::{machine_id, namespace_id, operation_id};

#[test]
fn server_configuration_matches_the_core_compatibility_surface() {
    let nats_config = external_server_config();
    let core_config = core_external_server_config();

    assert_eq!(nats_config.client_host(), core_config.client_host());
    assert_eq!(nats_config.port(), core_config.port());
    assert_eq!(nats_config.render(), core_config.render());

    let nats_loopback = NatsServerConfig::single_machine(
        machine_id("core_1"),
        NatsListener::Loopback,
        tls_files(),
        PathBuf::from("authorized-users.conf"),
    )
    .expect("valid loopback NATS server config");
    let core_loopback = ployz_core::nats_config::NatsServerConfig::single_machine(
        machine_id("core_1"),
        ployz_core::nats_config::NatsListener::Loopback,
        ployz_core::nats_config::NatsServerTlsFiles {
            cert_file: PathBuf::from("/var/lib/ployz/nats/server.crt"),
            key_file: PathBuf::from("/var/lib/ployz/nats/server.key"),
        },
        PathBuf::from("authorized-users.conf"),
    )
    .expect("valid Core compatibility loopback config");
    assert_eq!(nats_loopback.render(), core_loopback.render());
}

#[test]
fn authorization_and_permission_rendering_match_the_core_compatibility_surface() {
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
    assert_eq!(
        render_authorized_users(&grants),
        ployz_core::nats_config::render_authorized_users(&grants)
    );

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
        let core = ployz_core::permissions::NatsPermissionProfile::render(principal.clone());
        assert_eq!(nats.principal, core.principal);
        assert_eq!(
            nats.publish.allowed_subjects(),
            core.publish.allowed_subjects()
        );
        assert_eq!(
            nats.publish.denied_subjects(),
            core.publish.denied_subjects()
        );
        assert_eq!(
            nats.subscribe.allowed_subjects(),
            core.subscribe.allowed_subjects()
        );
        assert_eq!(
            nats.subscribe.denied_subjects(),
            core.subscribe.denied_subjects()
        );
        assert_eq!(
            matches!(
                nats.allow_responses,
                ployz_nats::permissions::ResponsePermission::Allowed
            ),
            matches!(
                core.allow_responses,
                ployz_core::permissions::ResponsePermission::Allowed
            )
        );
        assert_eq!(
            inbox_prefix(&principal),
            ployz_core::permissions::inbox_prefix(&principal)
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

fn core_external_server_config() -> ployz_core::nats_config::NatsServerConfig {
    ployz_core::nats_config::NatsServerConfig::single_machine(
        machine_id("core_1"),
        ployz_core::nats_config::NatsListener::External {
            advertise_host: ployz_core::nats_config::NatsAdvertisedHost::try_new(
                "core.example.test",
            )
            .expect("valid advertised host"),
        },
        ployz_core::nats_config::NatsServerTlsFiles {
            cert_file: PathBuf::from("/var/lib/ployz/nats/server.crt"),
            key_file: PathBuf::from("/var/lib/ployz/nats/server.key"),
        },
        PathBuf::from("authorized-users.conf"),
    )
    .expect("valid Core compatibility NATS server config")
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
