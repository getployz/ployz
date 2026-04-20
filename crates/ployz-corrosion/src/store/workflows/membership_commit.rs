use crate::client::CorrClient;
use crate::store::shared::sql::exec_all;
use crate::store::tables::{invites, machines};
use ployz_types::error::Result;
use ployz_types::model::MachineRecord;

pub(crate) async fn commit_membership(
    client: &CorrClient,
    record: &MachineRecord,
    invite_id: &str,
    now_unix_secs: u64,
) -> Result<()> {
    let statements = vec![
        machines::upsert_statement(record)?,
        invites::consume_statement(invite_id, now_unix_secs),
    ];
    exec_all(client, &statements, "commit_membership").await
}
