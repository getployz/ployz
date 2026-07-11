use ployz_sdk_types::{
    OperationEvent, PloyzNativeMeshMachineReady, PloyzNativeMeshPrepareReport,
    PloyzNativeMeshReady, WireGuardPublicKey, WireGuardReady,
};

#[test]
fn sdk_exports_network_repair_dataplane_prepared_event() {
    let report = PloyzNativeMeshPrepareReport::for_targets(
        &[ployz_sdk_types::MachineId::try_new("machine_a").expect("valid machine id")],
        [PloyzNativeMeshMachineReady {
            machine_id: ployz_sdk_types::MachineId::try_new("machine_a").expect("valid machine id"),
            ready: PloyzNativeMeshReady {
                wireguard: WireGuardReady {
                    public_key: WireGuardPublicKey::try_new("public-key-a")
                        .expect("valid wireguard public key"),
                    evidence: Vec::new(),
                },
                ebpf_forwarding: ployz_sdk_types::EbpfForwardingReady {
                    evidence: Vec::new(),
                },
            },
        }],
    )
    .expect("valid dataplane report");
    let event = OperationEvent::NetworkRepairDataplanePrepared {
        operation_id: ployz_sdk_types::OperationId::try_new("op_network_repair")
            .expect("valid operation id"),
        report,
    };

    assert_eq!(
        serde_json::to_string(&event).expect("event serializes"),
        r#"{"event":"network_repair_dataplane_prepared","operation_id":"op_network_repair","report":{"machines":[{"machine_id":"machine_a","wireguard":{"public_key":"public-key-a","evidence":[]},"ebpf_forwarding":{"evidence":[]}}]}}"#
    );
}
