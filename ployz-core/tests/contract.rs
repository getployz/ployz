use std::{
    collections::{BTreeMap, BTreeSet},
    net::Ipv4Addr,
    num::NonZeroU32,
};

use ployz_core::{
    CREATE_CONTAINER_CAPABILITY, CapabilityName, CodecError, ConfigMount, ConfigSpec,
    ConfiguredHealthcheck, ContainerCreated, ContainerHostname, ContainerId, ContainerKind,
    ContainerLabels, ContainerObservationMap, ContainerPath, ContainerResources,
    ContainerRuntimeObservation, ContractDescription, CreateContainerRequest,
    CreateDomainRecordsRequest, DESCRIBE_CONTRACT_CAPABILITY, DescribeContractRequest, DnsRecord,
    DnsRecordType, DockerVolumeName, Domain, DomainRecords, ENSURE_IMAGE_INGEST_CAPABILITY,
    EnsureImageIngestRequest, ExtraHost, FanoutFailure, FanoutOutcome, FanoutResponse,
    FramingError, GET_CONTAINER_OBSERVATIONS_CAPABILITY, GET_INGRESS_PROXY_CONFIG_CAPABILITY,
    GetContainerObservationsRequest, GetIngressProxyConfigRequest, HealthObservation,
    HealthcheckCommand, HealthcheckSpec, HttpProtocol, ImageIngestDestination, ImageIngestOpened,
    ImageIngestReason, ImagePulled, ImageSummary, IngressHost, IngressHostname, IngressProxyConfig,
    IngressProxyFragment, InspectWireGuardRequest, LIST_IMAGES_CAPABILITY, ListImagesRequest,
    MANAGED_LABEL, MachineFailure, MachineGateway, MachineId, MachineImages, MachineName,
    MachinePath, MachineRpc, MachineRpcClient, MachineRpcServer, MachineSubnet, MachineSuccess,
    MachineTarget, MachineTokenRequest, MachineUpdate, ManagementAddress, NameMatches,
    OpaquePayload, PROJECT_NAME_LABEL, PROTOCOL_MAJOR, PULL_IMAGE_FROM_MACHINE_CAPABILITY,
    PartialResult, Placement, PortPublication, PreDeployHook, ProjectName, PublicIpDiscovery,
    PublicIpUpdate, PullImageFromMachineRequest, PullPolicy, QualifiedService,
    RESET_MACHINE_CAPABILITY, RemoveLocalMachineRequest, RemoveMachineRequest,
    RequestedServiceSpec, ReserveDomainRequest, ResetAccepted, ResetRequest, ResolvedServiceSpec,
    ResponseKind, RestartPolicy, RpcError, RpcErrorCode, RpcRequestBody, RpcResponse,
    RpcResponseBody, ServiceContainerSpec, ServiceId, ServiceMode, ServiceMount, ServiceName,
    ServiceVolume, ServiceVolumeReference, UNRECOGNIZED_STATE, UpdateConfig, UpdateMachineRequest,
    UpdateOrder, VolumeSource, encode_grpc_frame, grpc_frames, op,
};
use prost::Message;
use serde_json::{Value, json};

const MACHINE_ID: &str = "0123456789abcdef0123456789abcdef";
const OTHER_MACHINE_ID: &str = "fedcba9876543210fedcba9876543210";

#[test]
fn ployz_owned_ports_use_the_fixed_ploy_range() {
    assert_eq!(
        [
            ployz_core::MACHINE_API_PORT,
            ployz_core::CORROSION_GOSSIP_PORT,
            ployz_core::CORROSION_API_PORT,
            ployz_core::UNREGISTRY_PORT,
        ],
        [7569, 7570, 7571, 7572]
    );
}

#[test]
fn provisioned_volume_sources_carry_required_positive_byte_counts() {
    let valid = json!({
        "kind": "provisioned",
        "name": "data",
        "maximum_bytes": "1073741824",
        "labels": {"backup": "daily"}
    });
    let source: VolumeSource = serde_json::from_value(valid.clone()).unwrap();
    assert_eq!(serde_json::to_value(source).unwrap(), valid);
    let exact_u64_max = json!({
        "kind": "provisioned",
        "name": "data",
        "maximum_bytes": "18446744073709551615",
        "labels": {}
    });
    let source: VolumeSource = serde_json::from_value(exact_u64_max.clone()).unwrap();
    assert_eq!(serde_json::to_value(source).unwrap(), exact_u64_max);
    for invalid in [
        r#"{"kind":"provisioned","name":"data"}"#,
        r#"{"kind":"provisioned","name":"data","maximum_bytes":"0"}"#,
        r#"{"kind":"provisioned","name":"data","maximum_bytes":"18446744073709551616"}"#,
        r#"{"kind":"provisioned","name":"data","maximum_bytes":9007199254740993}"#,
    ] {
        assert!(serde_json::from_str::<VolumeSource>(invalid).is_err());
    }
}

#[test]
fn service_volume_source_wire_forms_are_exact() {
    assert!(ployz_core::VolumeDriver::parse("ployz", BTreeMap::new()).is_err());

    for valid in [
        json!({"kind": "external", "name": "shared"}),
        json!({
            "kind": "ordinary",
            "name": "data",
            "driver": {"name": "local", "options": {}},
            "labels": {"backup": "daily"}
        }),
        json!({
            "kind": "provisioned",
            "name": "bounded",
            "maximum_bytes": "1073741824",
            "labels": {}
        }),
    ] {
        let source: VolumeSource = serde_json::from_value(valid.clone()).unwrap();
        assert_eq!(serde_json::to_value(source).unwrap(), valid);
    }

    for invalid in [
        json!({"kind": "named", "name": "data"}),
        json!({
            "kind": "ordinary",
            "name": "data",
            "driver": {"name": "ployz", "options": {}}
        }),
    ] {
        assert!(serde_json::from_value::<VolumeSource>(invalid).is_err());
    }
}

/// The response catalog generates `ResponseKind` from one table, so a typo in a row
/// would round-trip through both sides symmetrically and break only across versions.
/// This restates the wire strings independently.
#[test]
fn response_kinds_match_the_frozen_wire_contract() {
    let frozen = [
        (ResponseKind::ContractDescription, "contract_description"),
        (ResponseKind::MachineDetails, "machine_details"),
        (ResponseKind::MachineToken, "machine_token"),
        (ResponseKind::Initialized, "initialized"),
        (ResponseKind::Registered, "registered"),
        (ResponseKind::JoinAccepted, "join_accepted"),
        (ResponseKind::CloudPairingSet, "cloud_pairing_set"),
        (ResponseKind::MachineList, "machine_list"),
        (ResponseKind::ContainerList, "container_list"),
        (ResponseKind::ContainerDetails, "container_details"),
        (
            ResponseKind::ContainerObservationMap,
            "container_observation_map",
        ),
        (ResponseKind::ContainerCreated, "container_created"),
        (ResponseKind::ContainerChanged, "container_changed"),
        (ResponseKind::DockerVolume, "docker_volume"),
        (ResponseKind::CreateVolumeReport, "create_volume_report"),
        (ResponseKind::VolumeInventory, "volume_inventory"),
        (ResponseKind::VolumeRemoved, "volume_removed"),
        (ResponseKind::MachineImages, "machine_images"),
        (ResponseKind::ImageIngestOpened, "image_ingest_opened"),
        (ResponseKind::ImagePulled, "image_pulled"),
        (ResponseKind::IngressProxyConfig, "ingress_proxy_config"),
        (ResponseKind::Domain, "domain"),
        (ResponseKind::DomainRecords, "domain_records"),
        (ResponseKind::MachineUpdated, "machine_updated"),
        (ResponseKind::LocalMachineRemoved, "local_machine_removed"),
        (ResponseKind::MachineRemoved, "machine_removed"),
        (ResponseKind::WireGuardInspected, "wireguard_inspected"),
        (ResponseKind::ResetAccepted, "reset_accepted"),
        (ResponseKind::Error, "error"),
    ];
    for (kind, wire) in &frozen {
        assert_eq!(kind.as_str(), *wire);
        assert_eq!(
            serde_json::from_str::<ResponseKind>(&format!("\"{wire}\"")).unwrap(),
            *kind
        );
    }
}

#[test]
fn identities_validate_and_serialize_as_their_wire_strings() {
    let machine = MachineId::parse(MACHINE_ID).unwrap();
    assert_eq!(
        serde_json::to_string(&machine).unwrap(),
        format!("\"{MACHINE_ID}\"")
    );
    assert_eq!(
        serde_json::from_str::<MachineId>(&format!("\"{MACHINE_ID}\"")).unwrap(),
        machine
    );

    assert!(MachineId::parse("0123456789ABCDEF0123456789ABCDEF").is_err());
    assert!(ServiceId::parse("too-short").is_err());
    assert!(ServiceName::parse("Uppercase").is_err());
    assert!(ServiceName::parse("valid-service").is_ok());
}

#[test]
fn container_hostname_accepts_rfc1123_and_rejects_invalid_labels() {
    assert_eq!(
        ContainerHostname::parse("Shared.Host").unwrap().as_str(),
        "Shared.Host"
    );
    assert!(ContainerHostname::parse(format!("{}.b", "a".repeat(62))).is_ok());
    assert!(ContainerHostname::parse("bad_name").is_err());
    assert!(ContainerHostname::parse("-leading").is_err());
    assert!(ContainerHostname::parse(format!("{}.b", "a".repeat(63))).is_err());
}

#[test]
fn container_metadata_values_reject_invalid_wire_states() {
    assert_eq!(
        ExtraHost::from_parts("gateway", "host-gateway")
            .unwrap()
            .as_str(),
        "gateway:host-gateway"
    );
    assert_eq!(ExtraHost::parse("ipv6:::1").unwrap().as_str(), "ipv6:::1");
    assert_eq!(
        serde_json::from_str::<ExtraHost>(r#""ipv6:[::1]""#)
            .unwrap()
            .as_str(),
        "ipv6:::1"
    );
    assert_eq!(
        ExtraHost::from_parts("ipv6", "[::1]").unwrap().as_str(),
        "ipv6:::1"
    );
    assert!(ExtraHost::parse(":192.0.2.1").is_err());
    assert!(ExtraHost::parse("api:not-an-address").is_err());
    assert!(ExtraHost::from_parts("api:alias", "192.0.2.1").is_err());
    assert!(ExtraHost::from_parts("api alias", "192.0.2.1").is_err());
    assert!(
        ContainerLabels::parse(BTreeMap::from([("example.user".into(), "yes".into())])).is_ok()
    );
    assert!(
        ContainerLabels::parse(BTreeMap::from([("ployz.future".into(), "mine".into())])).is_err()
    );
}

#[test]
fn qualified_service_is_project_slash_name() {
    let identity = ployz_core::QualifiedService::parse("shop-staging/web").unwrap();
    assert_eq!(identity.project.as_str(), "shop-staging");
    assert_eq!(identity.name.as_str(), "web");
    assert_eq!(identity.to_string(), "shop-staging/web");
    assert_eq!(identity.dns_name(), "web.shop-staging");
    assert_eq!(
        ployz_core::QualifiedService::parse_dns_name(identity.dns_name()).unwrap(),
        identity
    );
    assert_eq!(
        ployz_core::QualifiedService::system_ingress().to_string(),
        "ployz-system/ingress"
    );
    assert_eq!(
        serde_json::to_string(&identity).unwrap(),
        "\"shop-staging/web\""
    );
    assert_eq!(
        serde_json::from_str::<ployz_core::QualifiedService>("\"shop-staging/web\"").unwrap(),
        identity
    );
    for invalid in ["web", "SHOP/web", "shop/web/extra", "/web", "shop/", ""] {
        assert!(
            ployz_core::QualifiedService::parse(invalid).is_err(),
            "{invalid}"
        );
    }
    for invalid in ["web", "web.SHOP", "web.shop.extra", ".web", "web.", ""] {
        assert!(
            ployz_core::QualifiedService::parse_dns_name(invalid).is_err(),
            "{invalid}"
        );
    }
}

#[test]
fn qualified_service_ingress_label_is_name_hyphen_project_under_63() {
    let shop = QualifiedService::parse("shop/web").unwrap();
    let blog = QualifiedService::parse("blog/web").unwrap();
    assert_eq!(shop.ingress_label().unwrap(), "web-shop");
    assert_eq!(blog.ingress_label().unwrap(), "web-blog");

    let name = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    let project = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let exact = QualifiedService::new(
        ProjectName::parse(project).unwrap(),
        ServiceName::parse(name).unwrap(),
    );
    assert_eq!(
        exact.ingress_label().unwrap(),
        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    );
}

#[test]
fn qualified_service_ingress_label_rejects_more_than_63_characters() {
    let name = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    let project = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let error = QualifiedService::new(
        ProjectName::parse(project).unwrap(),
        ServiceName::parse(name).unwrap(),
    )
    .ingress_label()
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        "generated Ingress Hostname label \"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\" exceeds the 63-character DNS label limit; shorten the Service Name or Project Name, or supply a custom hostname"
    );
}

#[test]
fn machine_name_rejects_spaces() {
    assert_eq!(
        MachineName::parse("BAD NAME").unwrap_err().to_string(),
        "invalid Machine Name \"BAD NAME\": a 1-63 character lowercase DNS label"
    );
}

#[test]
fn machine_name_rejects_uppercase_and_empty_strings() {
    assert_eq!(
        MachineName::parse("Vultr1").unwrap_err().to_string(),
        "invalid Machine Name \"Vultr1\": a 1-63 character lowercase DNS label"
    );
    assert_eq!(
        MachineName::parse("").unwrap_err().to_string(),
        "invalid Machine Name \"\": a 1-63 character lowercase DNS label"
    );
}

#[test]
fn machine_name_accepts_lowercase_dns_labels() {
    assert_eq!(MachineName::parse("vultr1").unwrap().as_str(), "vultr1");
    assert_eq!(
        MachineName::parse("machine-a").unwrap().as_str(),
        "machine-a"
    );
}

#[test]
fn project_name_accepts_lowercase_dns_labels() {
    assert_eq!(ProjectName::parse("shop").unwrap().as_str(), "shop");
    assert_eq!(ProjectName::parse("a1").unwrap().as_str(), "a1");
    assert_eq!(
        ProjectName::parse("shop-staging").unwrap().as_str(),
        "shop-staging"
    );
    assert_eq!(
        ProjectName::parse("a".repeat(63)).unwrap().as_str(),
        "a".repeat(63)
    );
    assert!(ProjectName::parse("ployz-system").unwrap().is_reserved());
    assert_eq!(ProjectName::system().as_str(), "ployz-system");
    assert!(!ProjectName::parse("shop").unwrap().is_reserved());
}

#[test]
fn project_volume_name_includes_the_project_and_differs_across_projects() {
    let logical = DockerVolumeName::parse("data").unwrap();
    assert_eq!(
        ProjectName::parse("shop-production")
            .unwrap()
            .volume_name(&logical)
            .as_str(),
        "shop-production_data"
    );
    assert_eq!(
        ProjectName::parse("shop-staging")
            .unwrap()
            .volume_name(&logical)
            .as_str(),
        "shop-staging_data"
    );
    assert_ne!(
        ProjectName::parse("shop-production")
            .unwrap()
            .volume_name(&logical),
        ProjectName::parse("shop-staging")
            .unwrap()
            .volume_name(&logical)
    );
}

#[test]
fn managed_volume_scope_to_project_is_idempotent_and_skips_external() {
    let project = ProjectName::parse("shop").unwrap();
    let mut named = VolumeSource::Ordinary {
        name: DockerVolumeName::parse("data").unwrap(),
        driver: ployz_core::VolumeDriver::parse("local", BTreeMap::new()).unwrap(),
        labels: Default::default(),
    };
    named.scope_to_project(&project);
    named.scope_to_project(&project);
    match &named {
        VolumeSource::Ordinary { name, labels, .. } => {
            assert_eq!(name.as_str(), "shop_data");
            assert_eq!(labels.get(MANAGED_LABEL), Some(&String::new()));
            assert_eq!(
                labels.get(PROJECT_NAME_LABEL).map(String::as_str),
                Some("shop")
            );
        }
        VolumeSource::External { .. }
        | VolumeSource::Provisioned { .. }
        | VolumeSource::Bind { .. }
        | VolumeSource::Tmpfs { .. } => {
            panic!("expected a named volume")
        }
    }

    let mut external = VolumeSource::External {
        name: DockerVolumeName::parse("shared").unwrap(),
    };
    external.scope_to_project(&project);
    match &external {
        VolumeSource::External { name } => {
            assert_eq!(name.as_str(), "shared");
        }
        VolumeSource::Ordinary { .. }
        | VolumeSource::Provisioned { .. }
        | VolumeSource::Bind { .. }
        | VolumeSource::Tmpfs { .. } => {
            panic!("expected a named volume")
        }
    }

    let mut foreign = VolumeSource::Ordinary {
        name: DockerVolumeName::parse("blog_data").unwrap(),
        driver: ployz_core::VolumeDriver::parse("local", BTreeMap::new()).unwrap(),
        labels: BTreeMap::from([(PROJECT_NAME_LABEL.into(), "blog".into())]),
    };
    foreign.scope_to_project(&project);
    match &foreign {
        VolumeSource::Ordinary { name, labels, .. } => {
            assert_eq!(name.as_str(), "blog_data");
            assert_eq!(
                labels.get(PROJECT_NAME_LABEL).map(String::as_str),
                Some("blog")
            );
        }
        VolumeSource::External { .. }
        | VolumeSource::Provisioned { .. }
        | VolumeSource::Bind { .. }
        | VolumeSource::Tmpfs { .. } => {
            panic!("expected a named volume")
        }
    }
}

#[test]
fn project_name_rejects_underscores_and_uppercase_without_normalising() {
    let expected =
        "a 1-63 character lowercase DNS label; underscores and uppercase are not accepted";
    for invalid in ["My_App", "SHOP", "shop_staging", "Shop", "-shop", "shop-"] {
        assert_eq!(
            ProjectName::parse(invalid).unwrap_err().to_string(),
            format!("invalid Project Name {invalid:?}: {expected}")
        );
    }
    assert_eq!(
        ProjectName::parse("").unwrap_err().to_string(),
        format!("invalid Project Name \"\": {expected}")
    );
    assert!(ProjectName::parse("a".repeat(64)).is_err());
    assert!(ProjectName::parse("shop.staging").is_err());
}

#[test]
fn ingress_host_rejects_empty_and_invalid_names() {
    assert_eq!(
        IngressHost::parse("").unwrap_err().to_string(),
        "invalid Ingress Hostname \"\": a 1-253 character lowercase DNS hostname"
    );
    assert!(IngressHost::parse("Example.com").is_err());
    assert!(IngressHost::parse("bad_host.example").is_err());
    assert!(IngressHost::parse(".example.com").is_err());
    assert_eq!(
        IngressHost::parse("app.example.com").unwrap().as_str(),
        "app.example.com"
    );
}

#[test]
fn machine_subnet_rejects_prefixes_that_are_not_slash_24() {
    assert_eq!(
        MachineSubnet::parse("10.210.0.0/16")
            .unwrap_err()
            .to_string(),
        "invalid Machine Subnet \"10.210.0.0/16\": an IPv4 /24 CIDR"
    );
    assert_eq!(
        MachineSubnet::parse("10.210.7.1/32")
            .unwrap_err()
            .to_string(),
        "invalid Machine Subnet \"10.210.7.1/32\": an IPv4 /24 CIDR"
    );
    assert_eq!(
        MachineSubnet::parse("not-a-cidr").unwrap_err().to_string(),
        "invalid Machine Subnet \"not-a-cidr\": an IPv4 /24 CIDR"
    );
    assert!(serde_json::from_str::<MachineSubnet>("\"10.210.0.0/16\"").is_err());
    assert!(MachineSubnet::try_from("10.210.0.0/16".parse::<ipnet::Ipv4Net>().unwrap()).is_err());
}

#[test]
fn machine_subnet_exposes_its_gateway_and_stays_a_cidr_string() {
    let subnet = MachineSubnet::parse("10.210.7.0/24").unwrap();
    assert_eq!(
        subnet.gateway(),
        MachineGateway(Ipv4Addr::new(10, 210, 7, 1))
    );
    assert_eq!(serde_json::to_string(&subnet).unwrap(), "\"10.210.7.0/24\"");
    assert_eq!(
        serde_json::from_str::<MachineSubnet>("\"10.210.7.0/24\"").unwrap(),
        subnet
    );
    assert_eq!(MachineSubnet::parse("10.210.7.5/24").unwrap(), subnet);
    assert_eq!(
        serde_json::to_string(&MachineSubnet::parse("10.210.7.5/24").unwrap()).unwrap(),
        "\"10.210.7.0/24\""
    );
}

#[test]
fn ingress_hostname_intent_is_cluster_domain_or_explicit() {
    assert_eq!(
        serde_json::to_value(IngressHostname::cluster_domain()).unwrap(),
        json!({ "kind": "cluster_domain" })
    );
    assert_eq!(
        serde_json::to_value(IngressHostname::cluster_domain_label("api").unwrap()).unwrap(),
        json!({ "kind": "cluster_domain", "label": "api" })
    );
    assert_eq!(
        serde_json::to_value(IngressHostname::explicit("app.example.com").unwrap()).unwrap(),
        json!({ "kind": "explicit", "hostname": "app.example.com" })
    );
    assert!(
        serde_json::from_value::<IngressHostname>(json!({ "kind": "assign_from_cluster_domain" }))
            .is_err()
    );
    assert!(
        serde_json::from_value::<PortPublication>(json!({
            "mode": "ingress",
            "hostname": "",
            "load_balancer_port": 80,
            "container_port": 8080,
            "http_protocol": "http"
        }))
        .is_err()
    );
    assert!(
        serde_json::from_value::<PortPublication>(json!({
            "mode": "ingress_transport",
            "container_port": 53,
            "transport_protocol": "udp"
        }))
        .is_err()
    );
    let assigned: PortPublication = serde_json::from_value(json!({
        "mode": "ingress",
        "hostname": { "kind": "cluster_domain" },
        "load_balancer_port": 80,
        "container_port": 8080,
        "http_protocol": "http"
    }))
    .unwrap();
    assert!(matches!(
        assigned,
        PortPublication::Ingress {
            hostname: IngressHostname::ClusterDomain { label: None },
            http_protocol: HttpProtocol::Http,
            ..
        }
    ));
    let chosen: PortPublication = serde_json::from_value(json!({
        "mode": "ingress",
        "hostname": { "kind": "cluster_domain", "label": "api" },
        "load_balancer_port": 80,
        "container_port": 8080,
        "http_protocol": "http"
    }))
    .unwrap();
    assert_eq!(
        chosen,
        PortPublication::Ingress {
            hostname: IngressHostname::cluster_domain_label("api").unwrap(),
            load_balancer_port: 80.try_into().unwrap(),
            container_port: 8080.try_into().unwrap(),
            http_protocol: HttpProtocol::Http,
        }
    );
    assert!(IngressHostname::cluster_domain_label("a".repeat(64)).is_err());
}

#[test]
fn duplicate_name_matches_remain_ambiguous() {
    let first = ServiceId::parse("11111111111111111111111111111111").unwrap();
    let second = ServiceId::parse("22222222222222222222222222222222").unwrap();

    assert_eq!(
        NameMatches::from_matches(vec![first, second]),
        NameMatches::Ambiguous(vec![first, second])
    );
}

#[test]
fn partial_results_keep_successes_failures_and_omissions_together() {
    let result = PartialResult {
        successes: vec![MachineSuccess {
            machine_id: MachineId::parse(MACHINE_ID).unwrap(),
            value: "running".to_owned(),
        }],
        failures: vec![MachineFailure {
            machine_id: MachineId::parse(OTHER_MACHINE_ID).unwrap(),
            error: "unreachable".to_owned(),
        }],
        omissions: vec![MachineId::parse("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap()],
    };

    assert!(!result.all_targets_succeeded());
    let round_trip: PartialResult<String, String> =
        serde_json::from_value(serde_json::to_value(&result).unwrap()).unwrap();
    assert_eq!(round_trip, result);
}

#[test]
fn unknown_observation_variants_preserve_the_raw_value() {
    let future = json!({
        "state": "hibernating",
        "wake_at": "tomorrow",
        "vendor": { "reason": 7 }
    });
    let observation: ContainerRuntimeObservation = serde_json::from_value(future.clone()).unwrap();

    assert_eq!(
        observation,
        ContainerRuntimeObservation::Unknown {
            raw: future.clone()
        }
    );
    let wrapped = json!({ "state": UNRECOGNIZED_STATE, "raw": future });
    assert_eq!(serde_json::to_value(&observation).unwrap(), wrapped);
    let reread: ContainerRuntimeObservation = serde_json::from_value(wrapped.clone()).unwrap();
    assert_eq!(reread, observation);

    // Docker's own inspect State object has no `state` key at all.
    let docker_state = json!({ "Status": "hibernating", "ExitCode": 0 });
    let observation: ContainerRuntimeObservation =
        serde_json::from_value(docker_state.clone()).unwrap();
    assert_eq!(
        observation,
        ContainerRuntimeObservation::Unknown { raw: docker_state }
    );

    let health: HealthObservation = serde_json::from_str("\"degraded\"").unwrap();
    assert_eq!(health, HealthObservation::Unrecognized("degraded".into()));

    let known_with_addition: ContainerRuntimeObservation = serde_json::from_value(json!({
        "state": "running",
        "health": "healthy",
        "engine_detail": "accepted and ignored"
    }))
    .unwrap();
    assert_eq!(
        known_with_addition,
        ContainerRuntimeObservation::Running {
            health: HealthObservation::Healthy
        }
    );
}

#[test]
fn unrecognized_wrappers_unwrap_to_the_observed_value() {
    // A newer reader recovers a state that crossed an older hop.
    let via_old_hop = json!({
        "state": UNRECOGNIZED_STATE,
        "raw": { "state": UNRECOGNIZED_STATE, "raw": { "state": "exited", "code": 3 } }
    });
    let observation: ContainerRuntimeObservation = serde_json::from_value(via_old_hop).unwrap();
    assert_eq!(observation, ContainerRuntimeObservation::Exited { code: 3 });

    // A wrapper without `raw` is kept whole rather than invented.
    let bare = json!({ "state": UNRECOGNIZED_STATE });
    let observation: ContainerRuntimeObservation = serde_json::from_value(bare.clone()).unwrap();
    assert_eq!(
        observation,
        ContainerRuntimeObservation::Unknown { raw: bare }
    );

    // Unwrapping is bounded, so hostile nesting cannot recurse without limit.
    let mut nested = json!({ "state": "future" });
    for _ in 0..64 {
        nested = json!({ "state": UNRECOGNIZED_STATE, "raw": nested });
    }
    let observation: ContainerRuntimeObservation = serde_json::from_value(nested).unwrap();
    assert!(matches!(
        observation,
        ContainerRuntimeObservation::Unknown { .. }
    ));
}

#[test]
fn additive_fields_are_accepted_on_requests_and_responses() {
    let request = OpaquePayload::new(
        serde_json::to_vec(&json!({
            "protocol_major": 1,
            "command": "describe_contract",
            "payload": {},
            "future_request_metadata": true
        }))
        .unwrap(),
    );
    assert_eq!(
        request.decode_request().unwrap(),
        op::DescribeContract::into_request(DescribeContractRequest {})
    );

    let response: RpcResponse = serde_json::from_value(json!({
        "protocol_major": 1,
        "kind": "future_response",
        "payload": { "answer": 42 },
        "future_response_metadata": true
    }))
    .unwrap();
    assert_eq!(
        response.kind(),
        ResponseKind::Unknown("future_response".into())
    );
    assert!(matches!(
        response.body,
        RpcResponseBody::Unknown { ref payload, .. }
            if payload == &json!({ "answer": 42 })
    ));
}

#[test]
fn unknown_commands_are_rejected_with_a_typed_error() {
    let payload = OpaquePayload::new(
        br#"{"protocol_major":1,"command":"future_mutation","payload":{}}"#.to_vec(),
    );

    assert!(matches!(
        payload.decode_request(),
        Err(CodecError::UnsupportedCommand(command)) if command == "future_mutation"
    ));
}

#[test]
fn incompatible_protocol_majors_are_rejected_explicitly() {
    let payload = OpaquePayload::new(
        br#"{"protocol_major":2,"command":"describe_contract","payload":{}}"#.to_vec(),
    );

    assert!(matches!(
        payload.decode_request(),
        Err(CodecError::UnsupportedProtocolMajor {
            requested: 2,
            supported: PROTOCOL_MAJOR
        })
    ));

    let response = OpaquePayload::new(
        br#"{"protocol_major":2,"kind":"future_response","payload":{}}"#.to_vec(),
    );
    assert!(matches!(
        response.decode_response(),
        Err(CodecError::UnsupportedProtocolMajor {
            requested: 2,
            supported: PROTOCOL_MAJOR
        })
    ));
}

#[test]
fn per_machine_capability_fixture_is_stable_and_additive() {
    let description = ContractDescription {
        machine_id: MachineId::parse(MACHINE_ID).unwrap(),
        protocol_major: PROTOCOL_MAJOR,
        daemon_version: "0.1.0".into(),
        capabilities: BTreeSet::from([
            CapabilityName::parse("ployz.containers.inspect.v1").unwrap(),
            CapabilityName::parse(DESCRIBE_CONTRACT_CAPABILITY).unwrap(),
        ]),
    };

    assert!(description.supports(DESCRIBE_CONTRACT_CAPABILITY));
    assert!(!description.supports("ployz.containers.replace.v2"));
    assert_eq!(
        serde_json::to_string(&description).unwrap(),
        concat!(
            "{\"machine_id\":\"0123456789abcdef0123456789abcdef\",",
            "\"protocol_major\":1,\"daemon_version\":\"0.1.0\",",
            "\"capabilities\":[\"ployz.containers.inspect.v1\",",
            "\"ployz.rpc.describe-contract.v1\"]}"
        )
    );

    let with_future_field: ContractDescription = serde_json::from_value(json!({
        "machine_id": MACHINE_ID,
        "protocol_major": 1,
        "daemon_version": "0.2.0",
        "capabilities": [DESCRIBE_CONTRACT_CAPABILITY, "vendor.future.contract.v1"],
        "build_revision": "future"
    }))
    .unwrap();
    assert!(with_future_field.supports("vendor.future.contract.v1"));
    assert!(CapabilityName::parse("future_contract").is_err());
}

#[test]
fn json_payload_round_trips_through_the_opaque_prost_envelope() {
    let request = op::DescribeContract::into_request(DescribeContractRequest {})
        .encode()
        .unwrap();
    let framed = request.encode_to_vec();
    let decoded = OpaquePayload::decode(framed.as_slice()).unwrap();

    assert_eq!(
        decoded.decode_request().unwrap(),
        op::DescribeContract::into_request(DescribeContractRequest {})
    );

    let response = RpcResponse {
        protocol_major: PROTOCOL_MAJOR,
        body: RpcResponseBody::Unknown {
            kind: "ployz.future.result.v1".into(),
            payload: Value::Array(vec![json!(1), json!(2)]),
        },
    };
    let response_payload = response.encode().unwrap();
    assert_eq!(
        response_payload.decode_json::<RpcResponse>().unwrap(),
        response
    );
}

#[test]
fn replicated_container_observation_contract_is_batched_and_complete() {
    let first = ContainerId::parse("a".repeat(64)).unwrap();
    let second = ContainerId::parse("b".repeat(64)).unwrap();
    let request = op::GetContainerObservations::into_request(GetContainerObservationsRequest {
        container_ids: vec![first, second],
        wait_millis: 2_000,
    });
    assert_eq!(request.encode().unwrap().decode_request().unwrap(), request);
    assert_eq!(request.body.command(), "get_container_observations");
    assert_eq!(
        GET_CONTAINER_OBSERVATIONS_CAPABILITY,
        "ployz.container.observations.v1"
    );

    let response = ContainerObservationMap {
        containers: BTreeMap::from([(first, None), (second, None)]),
    };
    assert_eq!(
        RpcResponse::from(response.clone())
            .encode()
            .unwrap()
            .decode_response()
            .unwrap()
            .decode::<op::GetContainerObservations>()
            .unwrap(),
        response
    );
}

#[test]
fn image_list_contract_keeps_machine_local_store_and_platforms() {
    let request = op::ListImages::into_request(ListImagesRequest {
        reference: Some("example.test/api:1.*".into()),
    });
    assert_eq!(request.encode().unwrap().decode_request().unwrap(), request);

    let images = MachineImages {
        containerd_store: true,
        images: vec![ImageSummary {
            id: "sha256:abcdef".into(),
            repo_tags: vec!["example.test/api:1.2".into()],
            created: 17,
            size: 42,
            containers: 1,
            platforms: vec!["linux/amd64".into(), "linux/arm64".into()],
        }],
    };
    let response = RpcResponse::from(images.clone());
    assert_eq!(
        response
            .encode()
            .unwrap()
            .decode_response()
            .unwrap()
            .decode::<op::ListImages>()
            .unwrap(),
        images
    );
    assert_eq!(LIST_IMAGES_CAPABILITY, "ployz.image.list.v1");
    assert_eq!(request.body.command(), "list_images");
    assert_ne!(request.body.command(), "ensure_image_ingest");
}

#[test]
fn image_ingest_contract_returns_the_management_address_destination() {
    let request = op::EnsureImageIngest::into_request(EnsureImageIngestRequest {});
    assert_eq!(request.encode().unwrap().decode_request().unwrap(), request);

    let opened = ImageIngestOpened {
        destination: ImageIngestDestination {
            management_address: ManagementAddress("fdcc::7".parse().unwrap()),
            port: ployz_core::UNREGISTRY_PORT,
        },
    };
    let response = RpcResponse::from(opened);
    assert_eq!(
        response
            .encode()
            .unwrap()
            .decode_response()
            .unwrap()
            .decode::<op::EnsureImageIngest>()
            .unwrap(),
        opened
    );
    assert_eq!(
        ENSURE_IMAGE_INGEST_CAPABILITY,
        "ployz.image.ingest.ensure.v1"
    );
    assert_eq!(request.body.command(), "ensure_image_ingest");
    assert_ne!(request.body.command(), "list_images");
    assert_eq!(opened.destination.port, ployz_core::UNREGISTRY_PORT);

    let frozen = [
        (ImageIngestReason::NotParticipating, "not_participating"),
        (ImageIngestReason::DockerUnavailable, "docker_unavailable"),
        (
            ImageIngestReason::UnsupportedContainerdStore,
            "unsupported_containerd_store",
        ),
        (
            ImageIngestReason::ContainerdSocketMissing,
            "containerd_socket_missing",
        ),
        (ImageIngestReason::StartFailed, "start_failed"),
    ];
    for (reason, wire) in frozen {
        assert_eq!(serde_json::to_value(reason).unwrap(), json!(wire));
        let error = reason.rpc_error("ingest unavailable");
        assert_eq!(
            ImageIngestReason::from_details(&error.details),
            Some(reason)
        );
        assert_eq!(error.details, json!({ "reason": wire }));
    }
    assert_eq!(
        ImageIngestReason::from_details(&json!({
            "reason": "docker_unavailable",
            "extra": 1
        })),
        Some(ImageIngestReason::DockerUnavailable)
    );
    assert_eq!(ImageIngestReason::from_details(&Value::Null), None);
}

#[test]
fn peer_image_pull_contract_names_the_source_management_destination() {
    let source = ImageIngestDestination {
        management_address: ManagementAddress("fdcc::7".parse().unwrap()),
        port: ployz_core::UNREGISTRY_PORT,
    };
    let request = op::PullImageFromMachine::into_request(PullImageFromMachineRequest {
        image: "busybox:1.37.0".into(),
        source,
    });
    assert_eq!(request.encode().unwrap().decode_request().unwrap(), request);
    assert_eq!(request.body.command(), "pull_image_from_machine");
    assert_ne!(request.body.command(), "ensure_image_ingest");
    assert_eq!(
        PULL_IMAGE_FROM_MACHINE_CAPABILITY,
        "ployz.image.pull-from-machine.v1"
    );

    let pulled = ImagePulled {};
    let response = RpcResponse::from(pulled);
    assert_eq!(
        response
            .encode()
            .unwrap()
            .decode_response()
            .unwrap()
            .decode::<op::PullImageFromMachine>()
            .unwrap(),
        pulled
    );
    assert_eq!(source.port, ployz_core::UNREGISTRY_PORT);
}

#[test]
fn ingress_proxy_config_contract_tags_the_exact_backend_file() {
    let request = op::GetIngressProxyConfig::into_request(GetIngressProxyConfigRequest {});
    assert_eq!(request.encode().unwrap().decode_request().unwrap(), request);

    let config = IngressProxyConfig::Caddy("example.test { respond ok }\n".into());
    let response = RpcResponse::from(config.clone());
    assert_eq!(
        response
            .encode()
            .unwrap()
            .decode_response()
            .unwrap()
            .decode::<op::GetIngressProxyConfig>()
            .unwrap(),
        config
    );
    assert_eq!(
        serde_json::to_value(&config).unwrap(),
        json!({ "backend": "caddy", "config": "example.test { respond ok }\n" })
    );
    assert_eq!(
        GET_INGRESS_PROXY_CONFIG_CAPABILITY,
        "ployz.ingress.config.v1"
    );
}

#[test]
fn hosted_dns_contract_keeps_credentials_daemon_side_and_records_exact() {
    let reserve = op::ReserveDomain::into_request(ReserveDomainRequest {
        endpoint: "https://dns.example/v1".into(),
    });
    assert_eq!(
        reserve.encode().unwrap().decode_request().unwrap().body,
        RpcRequestBody::ReserveDomain(ReserveDomainRequest {
            endpoint: "https://dns.example/v1".into(),
        })
    );

    let records = vec![
        DnsRecord {
            name: "*".into(),
            record_type: DnsRecordType::A,
            values: vec!["192.0.2.1".into()],
        },
        DnsRecord {
            name: "*".into(),
            record_type: DnsRecordType::Aaaa,
            values: vec!["2001:db8::1".into()],
        },
    ];
    assert_eq!(
        serde_json::to_value(&records).unwrap(),
        json!([
            { "name": "*", "type": "A", "values": ["192.0.2.1"] },
            { "name": "*", "type": "AAAA", "values": ["2001:db8::1"] }
        ])
    );
    let request = op::CreateDomainRecords::into_request(CreateDomainRecordsRequest {
        records: records.clone(),
    });
    assert_eq!(
        request.encode().unwrap().decode_request().unwrap().body,
        RpcRequestBody::CreateDomainRecords(CreateDomainRecordsRequest {
            records: records.clone(),
        })
    );

    assert_eq!(
        RpcResponse::from(Domain {
            name: "opaque.ployz.example".into()
        })
        .decode::<op::GetDomain>()
        .unwrap()
        .name,
        "opaque.ployz.example"
    );
    let created = vec![DnsRecord {
        name: "*.opaque.ployz.example".into(),
        record_type: DnsRecordType::A,
        values: vec!["192.0.2.1".into()],
    }];
    assert_eq!(
        RpcResponse::from(DomainRecords {
            records: created.clone()
        })
        .decode::<op::CreateDomainRecords>()
        .unwrap()
        .records,
        created
    );
}

#[test]
fn fanout_frames_preserve_opaque_messages_and_type_target_failures() {
    let machine_id = MachineId::parse(MACHINE_ID).unwrap();
    let machine_name = MachineName::parse("machine-a").unwrap();
    let original = encode_grpc_frame(br#"{\"future\":true}"#);
    let success = FanoutResponse::success(&machine_id, &machine_name, original.clone()).unwrap();
    let encoded = success.encode_grpc_frame();

    let frames = grpc_frames(&encoded).unwrap();
    let [outer] = frames.as_slice() else {
        panic!("expected one outer gRPC frame")
    };
    let decoded = FanoutResponse::decode_grpc_frame(outer).unwrap();
    assert_eq!(decoded.machine_id().unwrap(), machine_id);
    assert_eq!(decoded.machine_name().unwrap(), machine_name);
    assert!(matches!(
        decoded.outcome,
        Some(FanoutOutcome::FramedPayload(ref payload)) if payload == &original
    ));

    let failure = FanoutFailure {
        code: tonic::Code::Unavailable as u32,
        message: "connection refused".into(),
        details: vec![1, 2, 3],
    };
    let decoded = FanoutResponse::decode_grpc_frame(
        &FanoutResponse::failure(&machine_id, &machine_name, failure.clone()).encode_grpc_frame(),
    )
    .unwrap();
    assert_eq!(decoded.outcome, Some(FanoutOutcome::Failure(failure)));

    assert_eq!(grpc_frames(&[0, 0]), Err(FramingError::TruncatedHeader));
    assert_eq!(
        grpc_frames(&[0, 0, 0, 0, 4, 1, 2, 3]),
        Err(FramingError::TruncatedMessage {
            declared: 4,
            available: 3,
        })
    );
}

#[test]
fn stream_frames_round_trip_binary_exec_payloads_and_control_kinds() {
    use ployz_core::{ExecConfig, ExecOptions, ExecRequestFrame, ExecResponseFrame};

    let container_id = ployz_core::ContainerId::parse("a".repeat(64)).unwrap();
    let config = ExecRequestFrame::Config(ExecConfig {
        container_id,
        options: ExecOptions {
            command: vec!["printf".into(), "\\377".into()],
            attach_stdin: true,
            attach_stdout: true,
            attach_stderr: true,
            tty: false,
            detach: false,
        },
    });
    assert_eq!(
        ExecRequestFrame::decode(&config.encode().unwrap()).unwrap(),
        config
    );

    for request in [
        ExecRequestFrame::Stdin(Vec::new()),
        ExecRequestFrame::Stdin(vec![0, 0xff, b'\n']),
        ExecRequestFrame::Resize {
            width: 132,
            height: 43,
        },
    ] {
        assert_eq!(
            ExecRequestFrame::decode(&request.encode().unwrap()).unwrap(),
            request
        );
    }
    for response in [
        ExecResponseFrame::ExecId("exec-1".into()),
        ExecResponseFrame::Stdout(vec![0, 0xff]),
        ExecResponseFrame::Stderr(Vec::new()),
        ExecResponseFrame::Exit(42),
    ] {
        assert_eq!(
            ExecResponseFrame::decode(&response.encode().unwrap()).unwrap(),
            response
        );
    }
}

#[test]
fn streaming_requests_keep_typed_control_options_outside_raw_frames() {
    use ployz_core::{ContainerLogsRequest, LogsOptions, MachineLogService, MachineLogsRequest};

    let options = LogsOptions {
        follow: true,
        tail: -1,
        since_unix_seconds: Some(1_786_698_000),
        until_unix_seconds: Some(1_786_701_600),
    };
    for request in [
        op::ContainerLogs::into_request(ContainerLogsRequest {
            container_id: ployz_core::ContainerId::parse("c".repeat(64)).unwrap(),
            options: options.clone(),
        }),
        op::MachineLogs::into_request(MachineLogsRequest {
            service: MachineLogService::Ployz,
            options,
        }),
    ] {
        assert_eq!(request.encode().unwrap().decode_request().unwrap(), request);
    }
}

#[test]
fn log_frames_round_trip_each_body_and_keep_wire_kind_bytes() {
    use ployz_core::{LogBody, LogEntry};

    let metadata = log_frame_metadata();
    let stdout = LogEntry {
        metadata: metadata.clone(),
        timestamp_unix_nanos: 1_765_000_000_123_456_789,
        body: LogBody::Stdout(vec![0, 0xff, b'\n']),
    };
    let stderr = LogEntry {
        metadata: metadata.clone(),
        timestamp_unix_nanos: 1_765_000_000_123_456_789,
        body: LogBody::Stderr(vec![0, 0xff, b'\n']),
    };
    let heartbeat = LogEntry::heartbeat(metadata.clone(), 1_765_000_000_123_456_789);
    let error = LogEntry::error(metadata, "remote failed");

    for (entry, kind) in [
        (&stdout, 0x10),
        (&stderr, 0x11),
        (&heartbeat, 0x12),
        (&error, 0x13),
    ] {
        let encoded = entry.encode().unwrap();
        assert_eq!(encoded.json.first().copied(), Some(kind));
        assert_eq!(LogEntry::decode(&encoded).unwrap(), *entry);
    }
}

#[test]
fn log_frames_reject_desynced_kind_and_header() {
    use ployz_core::{LogBody, LogEntry, StreamProtocolError};

    let metadata = log_frame_metadata();
    let stdout = LogEntry {
        metadata: metadata.clone(),
        timestamp_unix_nanos: 1,
        body: LogBody::Stdout(vec![b'x']),
    };
    let error = LogEntry::error(metadata, "remote failed");

    for kind in [0x10, 0x11, 0x12] {
        let mut desynced = error.encode().unwrap();
        *desynced
            .json
            .first_mut()
            .expect("encoded log frames start with a kind byte") = kind;
        assert!(
            matches!(
                LogEntry::decode(&desynced),
                Err(StreamProtocolError::InvalidPayload {
                    kind: "log entry",
                    ..
                })
            ),
            "{kind:#04x}"
        );
    }
    let mut missing_error = stdout.encode().unwrap();
    *missing_error
        .json
        .first_mut()
        .expect("encoded log frames start with a kind byte") = 0x13;
    assert!(matches!(
        LogEntry::decode(&missing_error),
        Err(StreamProtocolError::InvalidPayload {
            kind: "log entry",
            ..
        })
    ));
    let mut heartbeat_with_message = stdout.encode().unwrap();
    *heartbeat_with_message
        .json
        .first_mut()
        .expect("encoded log frames start with a kind byte") = 0x12;
    assert!(matches!(
        LogEntry::decode(&heartbeat_with_message),
        Err(StreamProtocolError::InvalidPayload {
            kind: "log entry",
            ..
        })
    ));
}

fn log_frame_metadata() -> ployz_core::LogMetadata {
    use ployz_core::{LogMetadata, LogOrigin};

    LogMetadata {
        origin: LogOrigin::Service {
            service_id: ployz_core::ServiceId::parse("1".repeat(32)).unwrap(),
            service_name: ployz_core::ServiceName::parse("api").unwrap(),
            container_id: ployz_core::ContainerId::parse("b".repeat(64)).unwrap(),
            hook: Some("pre-deploy".into()),
        },
        machine_id: MachineId::parse(MACHINE_ID).unwrap(),
        machine_name: MachineName::parse("machine-a").unwrap(),
    }
}

#[test]
fn malformed_stream_frames_return_protocol_errors() {
    use ployz_core::{ExecRequestFrame, StreamProtocolError};

    assert_eq!(
        ExecRequestFrame::decode(&OpaquePayload::new(vec![1, 0, 0])),
        Err(StreamProtocolError::TruncatedHeader)
    );
    assert_eq!(
        ExecRequestFrame::decode(&OpaquePayload::new(vec![0xfe, 0, 0, 0, 0])),
        Err(StreamProtocolError::UnknownKind(0xfe))
    );
    assert_eq!(
        ExecRequestFrame::decode(&OpaquePayload::new(vec![2, 0, 0, 0, 3, 1, 2])),
        Err(StreamProtocolError::LengthMismatch {
            declared: 3,
            actual: 2,
        })
    );
}

#[test]
fn typed_errors_preserve_future_error_codes() {
    let error: RpcError = serde_json::from_value(json!({
        "code": "rate_limited",
        "message": "try later",
        "details": { "retry_after_seconds": 2 },
        "future_metadata": true
    }))
    .unwrap();

    assert_eq!(error.code, RpcErrorCode::Unknown("rate_limited".into()));
    let response = RpcResponse::from(error);
    assert_eq!(response.kind(), ResponseKind::Error);
    assert!(matches!(
        response.body,
        RpcResponseBody::Error(RpcError { ref details, .. })
            if details["retry_after_seconds"] == 2
    ));
}

#[test]
fn reset_command_and_acknowledgement_have_a_stable_capability() {
    let request = op::Reset::into_request(ResetRequest {}).encode().unwrap();
    assert_eq!(
        request.decode_request().unwrap(),
        op::Reset::into_request(ResetRequest {})
    );

    let response = RpcResponse::from(ResetAccepted {});
    assert_eq!(response.kind(), ResponseKind::ResetAccepted);
    assert!(response.decode::<op::Reset>().is_ok());
    assert_eq!(RESET_MACHINE_CAPABILITY, "ployz.machine.reset.v1");
}

#[test]
fn volume_and_container_commands_keep_machine_local_inputs_exact() {
    use std::collections::BTreeMap;

    use ployz_core::{
        CreateVolumeReport, CreateVolumeRequest, DockerVolume, DockerVolumeId, DockerVolumeName,
        InspectVolumeRequest, ListVolumesRequest, RemoveVolumeRequest, VolumeInventory,
    };

    assert!(DockerVolumeName::parse("").is_err());
    let name = DockerVolumeName::parse("data").unwrap();
    let create = CreateVolumeRequest {
        name: name.clone(),
        driver: "local".into(),
        options: BTreeMap::from([("type".into(), "none".into())]),
        labels: BTreeMap::from([("purpose".into(), "database".into())]),
    };
    assert_eq!(
        op::CreateVolume::into_request(create.clone())
            .encode()
            .unwrap()
            .decode_request()
            .unwrap()
            .body,
        RpcRequestBody::CreateVolume(create)
    );
    assert_eq!(
        op::ListVolumes::into_request(ListVolumesRequest {})
            .encode()
            .unwrap()
            .decode_request()
            .unwrap()
            .body,
        RpcRequestBody::ListVolumes(ListVolumesRequest {})
    );
    assert_eq!(
        op::InspectVolume::into_request(InspectVolumeRequest { name: name.clone() })
            .encode()
            .unwrap()
            .decode_request()
            .unwrap()
            .body,
        RpcRequestBody::InspectVolume(InspectVolumeRequest { name: name.clone() })
    );
    assert_eq!(
        op::RemoveVolume::into_request(RemoveVolumeRequest {
            name: name.clone(),
            force: true,
        })
        .encode()
        .unwrap()
        .decode_request()
        .unwrap()
        .body,
        RpcRequestBody::RemoveVolume(RemoveVolumeRequest {
            name: name.clone(),
            force: true,
        })
    );

    let volume = DockerVolume {
        id: DockerVolumeId {
            machine_id: MachineId::parse(MACHINE_ID).unwrap(),
            name,
        },
        options: BTreeMap::from([("type".into(), "none".into())]),
        labels: BTreeMap::from([("purpose".into(), "database".into())]),
        storage: ployz_core::DockerVolumeStorageObservation::Plain {
            driver: "local".into(),
        },
    };
    let volume_response = RpcResponse::from(CreateVolumeReport::Verified {
        volume: volume.clone(),
    });
    assert_eq!(volume_response.kind(), ResponseKind::CreateVolumeReport);
    assert_eq!(
        volume_response.decode::<op::CreateVolume>().unwrap(),
        CreateVolumeReport::Verified {
            volume: volume.clone()
        }
    );
    assert_eq!(
        RpcResponse::from(volume.clone())
            .decode::<op::InspectVolume>()
            .unwrap(),
        volume
    );
    assert_eq!(
        RpcResponse::from(VolumeInventory {
            volumes: vec![volume.clone()],
            failures: Vec::new(),
        })
        .decode::<op::ListVolumes>()
        .unwrap()
        .volumes,
        vec![volume]
    );
    let spec: ResolvedServiceSpec = serde_json::from_value(json!({
        "service_id": "11111111111111111111111111111111",
        "name": "api",
        "mode": { "mode": "replicated", "replicas": 1 },
        "container": { "image": "alpine:3.23.3", "pull_policy": "missing" }
    }))
    .unwrap();
    let request = op::CreateContainer::into_request(CreateContainerRequest {
        kind: ContainerKind::ServiceContainer,
        project_name: ProjectName::parse("shop").unwrap(),
        resolved_spec: spec.clone(),
    });
    assert_eq!(request.encode().unwrap().decode_request().unwrap(), request);
    assert!(
        serde_json::from_value::<CreateContainerRequest>(json!({
            "kind": "service_container",
            "resolved_spec": spec
        }))
        .is_err()
    );

    let created = ContainerCreated {
        container_id: ployz_core::ContainerId::parse("a".repeat(64)).unwrap(),
        display_name: "api-abcd".into(),
    };
    let response = RpcResponse::from(created.clone());
    assert_eq!(response.decode::<op::CreateContainer>().unwrap(), created);
    assert_eq!(CREATE_CONTAINER_CAPABILITY, "ployz.container.create.v1");
}

#[test]
fn remove_volumes_request_identifies_each_volume_by_machine_and_name() {
    use ployz_core::{DockerVolumeId, DockerVolumeName, RemoveVolumesRequest};

    assert!(
        serde_json::from_value::<RemoveVolumesRequest>(json!({ "volumes": ["data"] })).is_err()
    );
    assert!(
        serde_json::from_value::<RemoveVolumesRequest>(json!({
            "volumes": [{ "name": "data" }]
        }))
        .is_err()
    );
    let request: RemoveVolumesRequest = serde_json::from_value(json!({
        "volumes": [{
            "machine_id": MACHINE_ID,
            "name": "data"
        }]
    }))
    .unwrap();
    assert_eq!(
        request.volumes,
        vec![DockerVolumeId {
            machine_id: MachineId::parse(MACHINE_ID).unwrap(),
            name: DockerVolumeName::parse("data").unwrap(),
        }]
    );
    assert!(!request.force);
}

#[test]
fn machine_administration_requests_round_trip_as_typed_payloads() {
    let requests = [
        op::MachineToken::into_request(MachineTokenRequest {
            public_ip: PublicIpDiscovery::Override("203.0.113.7".parse().unwrap()),
            ..Default::default()
        }),
        op::UpdateMachine::into_request(UpdateMachineRequest {
            update: MachineUpdate {
                name: Some(MachineName::parse("renamed").unwrap()),
                public_ip: PublicIpUpdate::Remove,
                advertised_endpoints: None,
            },
        }),
        op::RemoveLocalMachine::into_request(RemoveLocalMachineRequest::default()),
        op::RemoveMachine::into_request(RemoveMachineRequest {
            machine_id: MachineId::parse(MACHINE_ID).unwrap(),
        }),
        op::InspectWireguard::into_request(InspectWireGuardRequest {}),
    ];
    for request in requests {
        assert_eq!(
            request.clone().encode().unwrap().decode_request().unwrap(),
            request
        );
    }
}

#[test]
fn requested_and_resolved_specs_and_mounts_round_trip() {
    let container = ServiceContainerSpec {
        image: "ghcr.io/example/api:sha".into(),
        command: vec!["serve".into()],
        entrypoint: Vec::new(),
        environment: Default::default(),
        labels: Default::default(),
        hostname: None,
        extra_hosts: Vec::new(),
        cap_add: vec!["NET_ADMIN".into()],
        cap_drop: Vec::new(),
        healthcheck: Some(HealthcheckSpec::Configured(ConfiguredHealthcheck {
            test: HealthcheckCommand::parse(["CMD", "true"]).unwrap(),
            interval_millis: Some(1_000),
            timeout_millis: None,
            start_period_millis: None,
            start_interval_millis: None,
            retries: Some(3),
        })),
        pull_policy: PullPolicy::Missing,
        init: None,
        user: None,
        working_directory: Some(ContainerPath::parse("/srv/app").unwrap()),
        tty: false,
        open_stdin: false,
        privileged: false,
        pid_mode: None,
        log_driver: None,
        resources: ContainerResources {
            memory_bytes: Some(256 * 1024 * 1024),
            ..Default::default()
        },
        stop_timeout_secs: Some(10),
        sysctls: Default::default(),
        restart: RestartPolicy::default(),
    };
    let reference = ServiceVolumeReference::parse("data").unwrap();
    let volume = ServiceVolume {
        reference: reference.clone(),
        source: VolumeSource::Bind {
            machine_path: MachinePath::parse("/srv/api").unwrap(),
            create_machine_path: true,
            propagation: None,
            recursive: None,
        },
    };
    let mount = ServiceMount {
        volume: reference,
        target: ContainerPath::parse("/var/lib/api").unwrap(),
        read_only: false,
        no_copy: false,
        subpath: None,
    };
    let requested = RequestedServiceSpec {
        name: ServiceName::parse("api").unwrap(),
        mode: ServiceMode::Replicated {
            replicas: NonZeroU32::new(2).unwrap(),
        },
        container: container.clone(),
        placement: Placement {
            machines: vec![MachineTarget::parse("edge").unwrap()],
        },
        ports: Vec::new(),
        volume_graph: ployz_core::ServiceVolumeGraph::parse(
            vec![volume.clone()],
            vec![mount.clone()],
        )
        .unwrap(),
        config_graph: ployz_core::ServiceConfigGraph::parse(
            vec![ConfigSpec {
                name: "settings".into(),
                content: b"port = 8080".to_vec(),
            }],
            vec![ConfigMount {
                config_name: "settings".into(),
                target: Some(ContainerPath::parse("/etc/api/settings.toml").unwrap()),
                uid: Some(1000),
                gid: Some(1000),
                mode: Some(0o440),
            }],
        )
        .unwrap(),
        pre_deploy: Some(PreDeployHook {
            command: vec!["migrate".into()],
            environment: Default::default(),
            privileged: None,
            timeout_millis: Some(30_000),
            user: None,
        }),
        ingress_proxy_fragment: Some(
            IngressProxyFragment::parse_caddy("reverse_proxy localhost:8080").unwrap(),
        ),
        update: UpdateConfig {
            order: None,
            monitor_millis: Some(5_000),
        },
    };
    let resolved = ResolvedServiceSpec {
        service_id: ServiceId::parse("11111111111111111111111111111111").unwrap(),
        name: requested.name.clone(),
        mode: requested.mode.clone(),
        container,
        placement: requested.placement.clone(),
        ports: Vec::new(),
        volume_graph: requested.volume_graph.clone(),
        config_graph: requested.config_graph.clone(),
        pre_deploy: requested.pre_deploy.clone(),
        ingress_proxy_fragment: requested.ingress_proxy_fragment.clone(),
        update: ployz_core::ResolvedUpdateConfig {
            order: UpdateOrder::StartFirst,
            monitor_millis: Some(5_000),
        },
    };

    let requested_json = serde_json::to_value(&requested).unwrap();
    let mut invalid_requested_json = requested_json.clone();
    *invalid_requested_json
        .pointer_mut("/container/labels")
        .expect("requested fixture has container labels") = json!({"ployz.future": "mine"});
    assert!(
        serde_json::from_value::<RequestedServiceSpec>(invalid_requested_json)
            .unwrap_err()
            .to_string()
            .contains("reserved 'ployz.*' management namespace")
    );
    assert_eq!(
        serde_json::from_value::<RequestedServiceSpec>(requested_json.clone()).unwrap(),
        requested
    );
    let mut older_requested_json = requested_json;
    older_requested_json
        .as_object_mut()
        .unwrap()
        .remove("update");
    let older_requested =
        serde_json::from_value::<RequestedServiceSpec>(older_requested_json).unwrap();
    assert_eq!(older_requested.update, UpdateConfig::default());
    assert_eq!(older_requested.container.restart, RestartPolicy::default());
    let resolved_json = serde_json::to_value(&resolved).unwrap();
    let mut invalid_resolved_json = resolved_json.clone();
    *invalid_resolved_json
        .pointer_mut("/container/labels")
        .expect("resolved fixture has container labels") = json!({"ployz.future": "mine"});
    assert!(
        serde_json::from_value::<ResolvedServiceSpec>(invalid_resolved_json)
            .unwrap_err()
            .to_string()
            .contains("reserved 'ployz.*' management namespace")
    );
    assert_eq!(
        serde_json::from_value::<ResolvedServiceSpec>(resolved_json.clone()).unwrap(),
        resolved
    );

    let mut dangling = serde_json::to_value(&resolved).unwrap();
    *dangling
        .get_mut("mounts")
        .and_then(Value::as_array_mut)
        .and_then(|mounts| mounts.first_mut())
        .and_then(|mount| mount.get_mut("volume"))
        .expect("fixture has a mount volume") = json!("missing");
    assert!(
        serde_json::from_value::<CreateContainerRequest>(json!({
            "kind": "service_container",
            "project_name": "shop",
            "resolved_spec": dangling
        }))
        .is_err()
    );
}

#[test]
fn service_ingress_proxy_fragment_is_backend_tagged_without_a_caddy_alias() {
    let spec: RequestedServiceSpec = serde_json::from_value(json!({
        "name": "api",
        "mode": { "mode": "replicated", "replicas": 1 },
        "container": { "image": "api:1", "pull_policy": "missing" },
        "ingress_proxy_fragment": {
            "backend": "caddy",
            "config": "  reverse_proxy localhost:8080\n"
        }
    }))
    .unwrap();

    let value = serde_json::to_value(spec).unwrap();
    assert_eq!(
        value.get("ingress_proxy_fragment"),
        Some(&json!({
            "backend": "caddy",
            "config": "reverse_proxy localhost:8080"
        }))
    );
    assert!(!value.as_object().unwrap().contains_key("caddy_config"));

    assert!(
        IngressProxyFragment::parse_caddy(" \n ")
            .unwrap_err()
            .to_string()
            .contains("non-empty configuration")
    );
    assert!(
        serde_json::from_value::<IngressProxyFragment>(json!({
            "backend": "caddy",
            "config": ""
        }))
        .unwrap_err()
        .to_string()
        .contains("non-empty configuration")
    );
}

// Catalog-driven compile witness for MachineRpc. Exec is absent from the catalog
// (bidirectional stream) and is wired by hand, same as ployz-core/build.rs.
macro_rules! compile_fixture {
    (
        package $package:literal
        unary { $($unary_variant:ident: ($unary_name:ident, $unary_route:literal, $unary_request:ty, $unary_command:literal, $unary_response:ty, $unary_capability:ident, $unary_capability_name:literal, $unary_advertisement:ident),)+ }
        server_streaming { $($stream_variant:ident: ($stream_name:ident, $stream_route:literal, $stream_request:ty, $stream_command:literal, $stream_capability:ident, $stream_capability_name:literal, $stream_advertisement:ident),)+ }
    ) => {
        struct CompileFixture;
        type EmptyRpcStream =
            tonic::codegen::tokio_stream::Empty<Result<OpaquePayload, tonic::Status>>;

        #[tonic::async_trait]
        impl MachineRpc for CompileFixture {
            type ExecStream = EmptyRpcStream;
            // ponytail: ident concat is unstable; name each catalog stream type here.
            type ContainerLogsStream = EmptyRpcStream;
            type MachineLogsStream = EmptyRpcStream;
            type RuntimeWatchStream = EmptyRpcStream;

            $(
                async fn $unary_name(
                    &self,
                    _request: tonic::Request<OpaquePayload>,
                ) -> Result<tonic::Response<OpaquePayload>, tonic::Status> {
                    unreachable!()
                }
            )+

            async fn exec(
                &self,
                _request: tonic::Request<tonic::Streaming<OpaquePayload>>,
            ) -> Result<tonic::Response<Self::ExecStream>, tonic::Status> {
                unreachable!()
            }

            $(
                async fn $stream_name(
                    &self,
                    _request: tonic::Request<OpaquePayload>,
                ) -> Result<tonic::Response<EmptyRpcStream>, tonic::Status> {
                    unreachable!()
                }
            )+
        }
    };
}

ployz_core::rpc_catalog!(compile_fixture);

#[test]
fn tonic_generates_both_sides_of_the_machine_rpc_service() {
    let _server = MachineRpcServer::new(CompileFixture);
    let _client: Option<MachineRpcClient<tonic::transport::Channel>> = None;
}
