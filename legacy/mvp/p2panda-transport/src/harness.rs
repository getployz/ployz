use mvp_bus::BusSession;
use mvp_p2panda_facts::{PandaFactExtensions, SharedPandaFactStore};
use p2panda_core::Operation;

use crate::{
    PandaNetFactImportOutcome, PandaNetTopic,
    fact_driver::import_p2panda_operation_into_shared_store,
};

pub async fn import_p2panda_operation(
    operation: Operation<PandaFactExtensions>,
    store: &SharedPandaFactStore,
    replica_session: &BusSession,
    topic: PandaNetTopic,
) -> PandaNetFactImportOutcome {
    import_p2panda_operation_into_shared_store(
        operation,
        store,
        replica_session,
        topic.into_inner(),
    )
    .await
}
