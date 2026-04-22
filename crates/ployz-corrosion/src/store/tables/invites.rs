use crate::client::CorrClient;
use crate::store::shared::decode::text;
use crate::store::shared::sql::{exec_all, query_rows};
use corro_api_types::{ExecResult, SqliteValue, Statement};
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
        _ => Err(Error::operation(
            "get_invite",
            "unexpected duplicate invite rows",
        )),
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

    match redeem_state(&invite, invite_id, machine_id, now_unix_secs)? {
        RedeemState::Replay => return Ok(invite),
        RedeemState::Available => {}
    }
    let mut next_invite = invite.clone();
    next_invite.consumed_by = Some(machine_id.clone());
    next_invite.consumed_at = Some(now_unix_secs);

    let compare_payload_json = payload_json(&invite, "redeem_invite")?;
    let invite_stmt = compare_and_swap_statement(&next_invite, &compare_payload_json)?;
    let res = client
        .execute(&[invite_stmt], None)
        .await
        .map_err(|e| Error::operation("redeem_invite", e.to_string()))?;
    match res.results.first() {
        Some(ExecResult::Execute {
            rows_affected: 1, ..
        }) => Ok(next_invite),
        Some(ExecResult::Execute {
            rows_affected: 0, ..
        }) => redeem_invite_after_compare_miss(client, invite_id, machine_id, now_unix_secs).await,
        Some(ExecResult::Execute { rows_affected, .. }) => Err(Error::operation(
            "redeem_invite",
            format!("unexpected rows affected: {rows_affected}"),
        )),
        Some(ExecResult::Error { error }) => Err(Error::operation("redeem_invite", error.clone())),
        None => Err(Error::operation("redeem_invite", "no result")),
    }
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
    let payload_json = payload_json(invite, "create_invite")?;
    Ok(Statement::WithParams(
        "INSERT INTO invites (invite_id, payload_json) VALUES (?, ?)".to_string(),
        vec![invite.invite_id.clone().into(), payload_json.into()],
    ))
}

fn update_statement(invite: &InviteRecord) -> Result<Statement> {
    let payload_json = payload_json(invite, "update_invite")?;
    Ok(Statement::WithParams(
        "UPDATE invites SET payload_json = ? WHERE invite_id = ?".to_string(),
        vec![payload_json.into(), invite.invite_id.clone().into()],
    ))
}

fn compare_and_swap_statement(
    invite: &InviteRecord,
    previous_payload_json: &str,
) -> Result<Statement> {
    let payload_json = payload_json(invite, "redeem_invite")?;
    Ok(Statement::WithParams(
        "UPDATE invites SET payload_json = ? WHERE invite_id = ? AND payload_json = ?".to_string(),
        vec![
            payload_json.into(),
            invite.invite_id.clone().into(),
            previous_payload_json.to_string().into(),
        ],
    ))
}

fn payload_json(invite: &InviteRecord, op: &'static str) -> Result<String> {
    serde_json::to_string(invite).map_err(|e| Error::operation(op, format!("serialize: {e}")))
}

async fn redeem_invite_after_compare_miss(
    client: &CorrClient,
    invite_id: &str,
    machine_id: &MachineId,
    now_unix_secs: u64,
) -> Result<InviteRecord> {
    let Some(current) = get_invite(client, invite_id).await? else {
        return Err(Error::operation(
            "invite_not_found",
            format!("invite '{invite_id}' not found"),
        ));
    };

    match redeem_state(&current, invite_id, machine_id, now_unix_secs)? {
        RedeemState::Replay => return Ok(current),
        RedeemState::Available => {}
    }

    Err(Error::operation(
        "redeem_invite",
        format!("invite '{invite_id}' changed during redemption"),
    ))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RedeemState {
    Available,
    Replay,
}

fn redeem_state(
    invite: &InviteRecord,
    invite_id: &str,
    machine_id: &MachineId,
    now_unix_secs: u64,
) -> Result<RedeemState> {
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
            return Ok(RedeemState::Replay);
        }
        return Err(Error::operation(
            "invite_consumed",
            format!("invite '{invite_id}' is already consumed"),
        ));
    }
    Ok(RedeemState::Available)
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

#[cfg(test)]
mod tests {
    use super::{RedeemState, redeem_state};
    use ployz_types::error::Error;
    use ployz_types::model::{InviteRecord, MachineId, NetworkId};

    fn invite() -> InviteRecord {
        InviteRecord {
            invite_id: "inv-1".into(),
            network_id: NetworkId("net-1".into()),
            issuer_machine_id: MachineId("issuer".into()),
            issuer_verify_key: "verify".into(),
            expires_at: 100,
            consumed_by: None,
            consumed_at: None,
            revoked_at: None,
            signature: "sig".into(),
        }
    }

    #[test]
    fn redeem_state_treats_same_machine_as_replay() {
        let mut invite = invite();
        invite.consumed_by = Some(MachineId("joiner".into()));

        let state = redeem_state(&invite, "inv-1", &MachineId("joiner".into()), 50)
            .expect("same machine replay should succeed");
        assert_eq!(state, RedeemState::Replay);
    }

    #[test]
    fn redeem_state_denies_other_machine_after_consumption() {
        let mut invite = invite();
        invite.consumed_by = Some(MachineId("joiner".into()));

        let result = redeem_state(&invite, "inv-1", &MachineId("other".into()), 50);
        assert!(matches!(
            result,
            Err(Error::Operation {
                operation: "invite_consumed",
                ..
            })
        ));
    }
}
