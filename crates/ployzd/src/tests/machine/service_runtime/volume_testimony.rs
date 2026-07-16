use super::*;
use crate::roles::machine::handle_volume_testimony;
use crate::roles::machine::protocol::{
    MachineVolumeTestimonyRpcRequest, MachineVolumeTestimonyRpcResponse,
};
use crate::roles::machine::runner::MachineVolumeUsageReader;
use ployz_nats::service_runtime::{NatsServiceRequest, NatsServiceResponse};

#[tokio::test]
async fn machine_volume_testimony_is_stable_drained_and_machine_fenced() {
    let nats = test_nats().await;
    let state = RecordingRunnerState::default();
    let available = VolumeUsageFacts {
        used_bytes: 4096,
        last_write_unix_seconds: 1_700_000_000,
    };
    let _service = start_machine_role_service(
        nats.machine_a.clone(),
        machine_id("machine_a"),
        RecordingRunner::new(state.clone()).with_volume_usage("alpha", available.clone()),
        ready_wireguard_ebpf(),
        idle_logs(),
    )
    .await
    .expect("machine runtime service starts");
    nats.machine_a.flush().await.expect("flush subscription");
    let alpha = pin("alpha", "machine_a");
    let zeta = pin("zeta", "machine_a");
    let reader = NatsMachineVolumeTestimonyReader::new(nats.client.clone())
        .with_request_timeout(Duration::from_secs(1));

    let testimony = reader
        .volume_testimony(
            &machine_id("machine_a"),
            vec![zeta.clone(), alpha.clone(), alpha.clone()],
        )
        .await
        .expect("volume testimony succeeds");
    assert_eq!(
        testimony,
        vec![
            MachineVolumeTestimonyResult {
                pin: alpha.clone(),
                testimony: MachineVolumeTestimony::Available { facts: available },
            },
            MachineVolumeTestimonyResult {
                pin: zeta.clone(),
                testimony: MachineVolumeTestimony::Unavailable,
            },
        ]
    );
    assert_eq!(state.volume_reads(), vec![alpha, zeta]);

    let wrong = pin("wrong", "machine_b");
    assert_eq!(
        reader
            .volume_testimony(&machine_id("machine_a"), vec![wrong])
            .await,
        Err(MachineVolumeTestimonyReadError::MachineMismatch {
            expected_machine_id: machine_id("machine_b"),
            responder_machine_id: machine_id("machine_a"),
        })
    );
    assert_eq!(state.volume_reads().len(), 2);
}

#[derive(Clone)]
struct DeadlineReader;

impl MachineVolumeUsageReader for DeadlineReader {
    async fn read_volume_usage(&self, volume: &VolumePinState) -> Option<VolumeUsageFacts> {
        if volume.volume_name().as_str() == "volume-00" {
            return Some(VolumeUsageFacts {
                used_bytes: 1,
                last_write_unix_seconds: 2,
            });
        }
        std::future::pending().await
    }
}

#[tokio::test(start_paused = true)]
async fn testimony_deadline_preserves_completed_reads_and_drains_more_than_one_wave() {
    let pins = (0..17)
        .map(|index| pin(&format!("volume-{index:02}"), "machine_a"))
        .collect::<Vec<_>>();
    let request = NatsServiceRequest {
        headers: None,
        payload: serde_json::to_vec(&MachineVolumeTestimonyRpcRequest { pins: pins.clone() })
            .expect("request encodes"),
    };

    let response = handle_volume_testimony(machine_id("machine_a"), DeadlineReader, request).await;
    let NatsServiceResponse::Ok { payload } = response else {
        panic!("testimony should answer successfully");
    };
    let MachineVolumeTestimonyRpcResponse::Ok(ok) =
        serde_json::from_slice(&payload).expect("response decodes")
    else {
        panic!("testimony should be a domain success");
    };
    assert_eq!(ok.results.len(), pins.len());
    let [first_result, remaining_results @ ..] = ok.results.as_slice() else {
        panic!("testimony must include every requested pin");
    };
    let [first_pin, ..] = pins.as_slice() else {
        panic!("test requires at least one requested pin");
    };
    assert_eq!(&first_result.pin, first_pin);
    assert!(matches!(
        &first_result.testimony,
        MachineVolumeTestimony::Available { .. }
    ));
    assert!(
        remaining_results
            .iter()
            .all(|result| result.testimony == MachineVolumeTestimony::Unavailable)
    );
}

fn pin(volume: &str, machine: &str) -> VolumePinState {
    VolumePinState::plain(
        namespace_id("default"),
        VolumeName::try_new(volume).expect("volume"),
        machine_id(machine),
    )
}
