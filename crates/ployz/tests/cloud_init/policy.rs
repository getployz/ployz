//! Enrollment refuses policy changes on an already assigned Machine.

use super::*;

#[tokio::test]
async fn cloud_resume_refuses_policy_mismatch_without_editing_the_machine() {
    for resume_founder in [true, false] {
        let mut founder = founder_machine();
        founder.accepts_services = false;
        founder.accepts_ingress = false;
        let registration = Registered {
            assigned_machine: founder.clone(),
            visible_peers: Vec::new(),
            target_versions: Default::default(),
        };
        let relay = RelayListen::start().await;
        let pairing =
            CloudPairing::parse(&relay.url, PairingCredential::parse(PAIRING).unwrap()).unwrap();
        let response = if resume_founder {
            json!({"kind": "initialize", "resumed": true, "storage": "none", "pairing": pairing})
        } else {
            json!({"kind": "join", "storage": "none", "pairing": pairing, "registration": registration})
        };
        let enroll = EnrollListen::start(response).await;
        let events = EventLog::default();
        let daemon = JoinDaemon::new(registration).with_events(events.clone());
        let address = serve_machine(daemon.clone()).await;
        connect_daemon(address)
            .await
            .call::<op::Initialize>(
                InitializeRequest {
                    initial_policy: ployz_core::InitialMachinePolicy {
                        accepts_services: false,
                        accepts_ingress: false,
                        ..Default::default()
                    },
                    name: founder.name,
                    cluster_network: "10.210.0.0/16".parse().unwrap(),
                    public_ip: None,
                    advertised_endpoints: founder.advertised_endpoints,
                    wireguard_mtu: None,
                    cloud_pairing: None,
                },
                None,
            )
            .await
            .unwrap();
        let output = init_cloud(
            &format!("tcp://{address}"),
            &enroll.url,
            "founder",
            false,
            true,
        )
        .await;
        assert!(
            !output.status.success(),
            "enrollment must not edit a participating Machine"
        );
        assert!(String::from_utf8_lossy(&output.stderr).contains("initial policy differs"));
        let observed = connect_daemon(address)
            .await
            .call::<op::Inspect>(InspectRequest::default(), None)
            .await
            .unwrap()
            .machine
            .unwrap();
        assert!(!observed.accepts_services);
        assert!(!observed.accepts_ingress);
        assert_eq!(events.entries(), ["initialize"]);
        assert_eq!(daemon.initialize_requests().len(), 1);
    }
}

#[tokio::test]
async fn machine_init_and_add_send_policy_in_creation_without_an_update() {
    use ployz::context::{Config, Connection, Context};
    use std::collections::BTreeMap;

    for adding in [false, true] {
        let mut registration = registration();
        registration.assigned_machine.labels =
            [("pool".parse().unwrap(), "build".parse().unwrap())].into();
        registration.assigned_machine.accepts_services = false;
        registration.assigned_machine.accepts_ingress = false;
        let events = EventLog::default();
        let entry = JoinDaemon::new(registration.clone()).with_events(events.clone());
        let target = JoinDaemon::new(registration).with_events(events.clone());
        let entry_address = serve_machine(entry.clone()).await;
        let target_address = serve_machine(target.clone()).await;
        let root = std::env::temp_dir().join(format!(
            "ployz-cli-initial-policy-{}",
            ployz_core::MachineId::random()
        ));
        let config = root.join("config.yaml");
        if adding {
            Config::new(
                &config,
                Some("test".into()),
                BTreeMap::from([(
                    "test".into(),
                    Context {
                        connections: vec![Connection::tcp(entry_address)],
                    },
                )]),
            )
            .save()
            .unwrap();
        }
        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_ployz"));
        command.args([
            "--ployz-config",
            config.to_str().unwrap(),
            "machine",
            if adding { "add" } else { "init" },
            &format!("tcp://{target_address}"),
            "--no-install",
            "--name",
            "builder",
            "--label-add",
            "pool=build",
            "--accepts-services=false",
            "--accepts-ingress=false",
            "--yes",
        ]);
        if !adding {
            command.arg("--no-dns");
        }
        let output = command.output().await.unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let policy = if adding {
            entry.register_request().initial_policy
        } else {
            target.initialize_request().initial_policy
        };
        assert!(!policy.accepts_services);
        assert!(!policy.accepts_ingress);
        assert!(policy.accepts_builds);
        assert_eq!(policy.labels.get("pool").unwrap().as_str(), "build");
        let observed = connect_daemon(target_address)
            .await
            .call::<op::Inspect>(InspectRequest::default(), None)
            .await
            .unwrap()
            .machine
            .unwrap();
        assert!(policy.matches(&observed));
        assert!(!events.entries().contains(&"update_machine"));
        std::fs::remove_dir_all(root).unwrap();
    }
}
