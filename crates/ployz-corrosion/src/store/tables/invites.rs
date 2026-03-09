use crate::client::CorrClient;
use crate::store::shared::sql::query_rows;
use corro_api_types::{ExecResult, Statement};
use ployz_sdk::error::{Error, Result};
use ployz_sdk::model::InviteRecord;

pub(crate) async fn create_invite(client: &CorrClient, invite: &InviteRecord) -> Result<()> {
    let stmt = Statement::WithParams(
        "INSERT INTO invites (id, expires_at) VALUES (?, ?)".to_string(),
        vec![invite.id.clone().into(), (invite.expires_at as i64).into()],
    );
    let res = client
        .execute(&[stmt], None)
        .await
        .map_err(|e| Error::operation("create_invite", e.to_string()))?;
    match res.results.first() {
        Some(ExecResult::Error { error }) if error.contains("UNIQUE") => {
            Err(Error::operation("invite_exists", error.clone()))
        }
        Some(ExecResult::Error { error }) => Err(Error::operation("create_invite", error.clone())),
        Some(ExecResult::Execute { .. }) => Ok(()),
        None => Err(Error::operation("create_invite", "no result")),
    }
}

pub(crate) async fn consume_invite(
    client: &CorrClient,
    invite_id: &str,
    now_unix_secs: u64,
) -> Result<()> {
    let stmt = Statement::WithParams(
        "DELETE FROM invites WHERE id = ? AND expires_at >= ?".to_string(),
        vec![invite_id.to_string().into(), (now_unix_secs as i64).into()],
    );
    let res = client
        .execute(&[stmt], None)
        .await
        .map_err(|e| Error::operation("consume_invite", e.to_string()))?;

    match res.results.first() {
        Some(ExecResult::Execute { rows_affected, .. }) if *rows_affected == 1 => Ok(()),
        Some(ExecResult::Error { error }) => Err(Error::operation("consume_invite", error.clone())),
        _ => {
            let lookup = Statement::WithParams(
                "SELECT id, expires_at FROM invites WHERE id = ? LIMIT 1".to_string(),
                vec![invite_id.to_string().into()],
            );
            if query_rows(client, &lookup, "consume_invite")
                .await?
                .is_empty()
            {
                Err(Error::operation(
                    "invite_not_found",
                    format!("invite '{invite_id}' not found"),
                ))
            } else {
                Err(Error::operation(
                    "invite_expired",
                    format!("invite '{invite_id}' is expired"),
                ))
            }
        }
    }
}
