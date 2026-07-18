use std::time::Duration;

use crate::control::role_client::machine::{
    MAX_CONCURRENT_MACHINE_READS, NatsMachineFactsReader,
    read_machine_placement_facts_with_gather_timeout,
};
use crate::roles::machine::protocol::{
    MachineBuildCapability, MachineFactsGetDomainError, MachineFactsGetRpcOk, MachineRpcResponse,
};
use crate::tests::support::nats::TestNats;
use futures_util::StreamExt;
use ployz_core::ids::MachineId;
use ployz_core::image::OciPlatform;
use ployz_core::machine::MachineLifecycle;
use ployz_core::machine::runtime::{
    MachineContainerObservationSnapshot, MachineDiskSpace, MachineFactsSnapshot,
};
use ployz_nats::subjects::{MachineServiceEndpoint, machine_service};

#[tokio::test]
async fn bounded_placement_gather_queries_every_candidate_before_classifying_silence() {
    let candidates = (0_u8..17)
        .map(|index| {
            (
                MachineId::try_new(format!("machine_{index:02}")).expect("candidate machine id"),
                MachineLifecycle::Active,
            )
        })
        .collect::<Vec<_>>();
    let machine_ids = candidates
        .iter()
        .map(|(machine_id, _)| machine_id.clone())
        .collect::<Vec<_>>();
    let nats = TestNats::start_with_machines(&machine_ids).await;
    let mut responders = Vec::new();

    for (index, machine_id) in machine_ids.iter().cloned().enumerate() {
        let client = nats.machine_client(&machine_id).await;
        let subject = machine_service(&machine_id, MachineServiceEndpoint::FactsGet);
        let mut subscriber = client
            .subscribe(subject)
            .await
            .expect("subscribe facts service");
        client
            .flush()
            .await
            .expect("flush facts service subscription");
        responders.push(tokio::spawn(async move {
            let request = subscriber.next().await.expect("facts request");

            if index == MAX_CONCURRENT_MACHINE_READS {
                let reply = request.reply.expect("facts request reply subject");
                let facts = MachineFactsSnapshot::try_new(
                    machine_id.clone(),
                    MachineContainerObservationSnapshot::try_new(machine_id.clone(), [])
                        .expect("containers"),
                    None,
                    MachineDiskSpace {
                        available_bytes: 1,
                        total_bytes: 2,
                    },
                    None,
                    OciPlatform::try_new("linux", "amd64").expect("platform"),
                    1_234,
                )
                .expect("facts");
                let response =
                    MachineRpcResponse::<MachineFactsGetRpcOk, MachineFactsGetDomainError>::Ok(
                        MachineFactsGetRpcOk {
                            facts,
                            build: MachineBuildCapability::Available,
                        },
                    );
                client
                    .publish(
                        reply,
                        serde_json::to_vec(&response)
                            .expect("facts response serializes")
                            .into(),
                    )
                    .await
                    .expect("publish facts response");
                client.flush().await.expect("flush facts response");
            }
        }));
    }

    let gathered = read_machine_placement_facts_with_gather_timeout(
        &NatsMachineFactsReader::new(nats.controller_client()),
        candidates,
        Duration::from_millis(500),
    )
    .await;
    let responder_results = tokio::time::timeout(
        Duration::from_secs(2),
        futures_util::future::join_all(responders),
    )
    .await
    .expect("every candidate receives a facts request");
    for result in responder_results {
        result.expect("facts responder exits cleanly");
    }

    assert_eq!(gathered.len(), machine_ids.len());
    for (index, (facts, machine_id)) in gathered.into_iter().zip(machine_ids.iter()).enumerate() {
        assert_eq!(&facts.machine_id, machine_id);
        assert_eq!(facts.lifecycle, MachineLifecycle::Active);
        assert_eq!(
            facts.answer.is_some(),
            index == MAX_CONCURRENT_MACHINE_READS
        );
    }
}
