use crate::client::CorrClient;
use crate::store::shared::decode::text;
use crate::store::shared::sql::{exec_all, query_rows};
use corro_api_types::{SqliteValue, Statement};
use ployz_types::error::{Error, Result};
use ployz_types::model::{InviteRecord, MachineId};

const SQL_LIST_INVITES: &str =
    "SELECT invite_id, payload_json FROM invites WHERE payload_json <> '' ORDER BY invite_id";

pub(crate) async fn create_invite(client: &CorrClient, invite: &InviteRecord) -> Result<()> {
    let stmt = insert_statement(invite)?;
    exec_all(client, &[stmt], "create_invite").await
}

pub(crate) async fn get_invite(
    client: &CorrClient,
    invite_id: &str,
) -> Result<Option<InviteRecord>> {
    let stmt = Statement::WithParams(
        "SELECT invite_id, payload_json FROM invites WHERE invite_id = ? AND payload_json <> '' LIMIT 1"
            .to_string(),
        vec![invite_id.to_string().into()],
    );
    let rows = query_rows(client, &stmt, "get_invite").await?;
    match rows.as_slice() {
        [] => Ok(None),
        [row] => parse_invite_row(row).map(Some),
        _ => Err(Error::operation("get_invite", "unexpected duplicate invite rows")),
    }
}

pub(crate) async fn list_invites(client: &CorrClient) -> Result<Vec<InviteRecord>> {
    let stmt = Statement::Simple(SQL_LIST_INVITES.to_string());
    query_rows(client, &stmt, "list_invites")
        .await?
        .iter()
        .map(|row| parse_invite_row(row))
        .collect()
}

pub(crate) async fn redeem_invite(
    client: &CorrClient,
    invite_id: &str,
    machine_id: &MachineId,
    now_unix_secs: u64,
) -> Result<InviteRecord> {
    let Some(invite) = get_invite(client, invite_id).await? else {
        return Err(Error::operation(
            "invite_not_found",
            format!("invite '{invite_id}' not found"),
        ));
    };

    if invite.revoked_at.is_some() {
        return Err(Error::operation(
            "invite_revoked",
            format!("invite '{invite_id}' is revoked"),
        ));
    }
    if now_unix_secs > invite.expires_at {
        return Err(Error::operation(
            "invite_expired",
            format!("invite '{invite_id}' is expired"),
        ));
    }
    if let Some(consumed_by) = &invite.consumed_by {
        if consumed_by == machine_id {
            return Ok(invite);
        }
        return Err(Error::operation(
            "invite_consumed",
            format!("invite '{invite_id}' is already consumed"),
        ));
    }

    let mut next_invite = invite.clone();
    next_invite.consumed_by = Some(machine_id.clone());
    next_invite.consumed_at = Some(now_unix_secs);

    let invite_stmt = update_statement(&next_invite)?;
    exec_all(client, &[invite_stmt], "redeem_invite").await?;
    Ok(next_invite)
}

pub(crate) async fn revoke_invite(
    client: &CorrClient,
    invite_id: &str,
    now_unix_secs: u64,
) -> Result<InviteRecord> {
    let Some(invite) = get_invite(client, invite_id).await? else {
        return Err(Error::operation(
            "invite_not_found",
            format!("invite '{invite_id}' not found"),
        ));
    };

    if invite.consumed_by.is_some() {
        return Err(Error::operation(
            "invite_consumed",
            format!("invite '{invite_id}' is already consumed"),
        ));
    }

    let mut next_invite = invite.clone();
    next_invite.revoked_at = Some(now_unix_secs);
    let stmt = update_statement(&next_invite)?;
    exec_all(client, &[stmt], "revoke_invite").await?;
    Ok(next_invite)
}

fn insert_statement(invite: &InviteRecord) -> Result<Statement> {
    let payload_json = serde_json::to_string(invite)
        .map_err(|e| Error::operation("create_invite", format!("serialize: {e}")))?;
    Ok(Statement::WithParams(
        "INSERT INTO invites (invite_id, payload_json) VALUES (?, ?)".to_string(),
        vec![invite.invite_id.clone().into(), payload_json.into()],
    ))
}

fn update_statement(invite: &InviteRecord) -> Result<Statement> {
    let payload_json = serde_json::to_string(invite)
        .map_err(|e| Error::operation("update_invite", format!("serialize: {e}")))?;
    Ok(Statement::WithParams(
        "UPDATE invites SET payload_json = ? WHERE invite_id = ?".to_string(),
        vec![payload_json.into(), invite.invite_id.clone().into()],
    ))
}

fn parse_invite_row(row: &[SqliteValue]) -> Result<InviteRecord> {
    let [invite_id_val, payload_val] = row else {
        return Err(Error::operation(
            "parse_invite_row",
            format!("expected 2 columns, got {}", row.len()),
        ));
    };

    let invite_id = text(invite_id_val, "invite_id")?;
    let payload_json = text(payload_val, "payload_json")?;
    let invite: InviteRecord = serde_json::from_str(&payload_json)
        .map_err(|e| Error::operation("parse_invite_row", format!("decode payload: {e}")))?;
    if invite.invite_id != invite_id {
        return Err(Error::operation(
            "parse_invite_row",
            "invite key mismatch between row and payload",
        ));
    }
    Ok(invite)
}
