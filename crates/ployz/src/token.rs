//! Join-token command execution and human presentation.

use hyper::Method;
use ployz_core::ids::TokenName;
use ployz_core::join::{
    TokenCreateRefusal, TokenCreateReply, TokenCreateRequest, TokenListReply, TokenListRequest,
    TokenListScope, TokenRevokeRefusal, TokenRevokeReply, TokenRevokeRequest,
};
use ployz_core::{TOKEN_CREATE_ROUTE, TOKEN_LIST_ROUTE, token_revoke_route};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::commands::{TokenCommand, TokenCreateCommand, TokenListCommand, TokenRevokeCommand};
use crate::mesh::http::JsonReply;
use crate::remote::{OperatorRemote, OperatorRemoteError};

pub async fn execute(command: TokenCommand) -> Result<String, TokenExecutionError> {
    match command {
        TokenCommand::Create(command) => create(command).await,
        TokenCommand::List(command) => list(command).await,
        TokenCommand::Revoke(command) => revoke(command).await,
    }
}

async fn create(command: TokenCreateCommand) -> Result<String, TokenExecutionError> {
    let remote = OperatorRemote::load(command.target.as_ref())?;
    let reply = remote
        .request_json_with_refusal::<_, TokenCreateReply, TokenCreateRefusal>(
            Method::POST,
            TOKEN_CREATE_ROUTE,
            Some(&TokenCreateRequest {
                name: command.name,
                ttl_seconds: command.ttl,
            }),
        )
        .await?;
    let reply = match reply {
        JsonReply::Success(reply) => reply,
        JsonReply::Refused(TokenCreateRefusal::NoAdvertisedDoorEndpoint { repair_command }) => {
            return Err(TokenExecutionError::NoAdvertisedDoorEndpoint { repair_command });
        }
        JsonReply::Refused(TokenCreateRefusal::TooManyAdvertisedDoorEndpoints {
            found,
            maximum,
        }) => {
            return Err(TokenExecutionError::TooManyAdvertisedDoorEndpoints { found, maximum });
        }
    };
    Ok(render_token_create(
        &reply.token_id,
        reply.blob.expose(),
        &render_absolute_utc(&reply.expires_at.to_string()),
        u64::from(command.ttl.get()),
    ))
}

async fn list(command: TokenListCommand) -> Result<String, TokenExecutionError> {
    let remote = OperatorRemote::load(command.target.as_ref())?;
    let reply: TokenListReply = remote
        .request_json(
            Method::POST,
            TOKEN_LIST_ROUTE,
            Some(&TokenListRequest {
                scope: if command.include_expired {
                    TokenListScope::All
                } else {
                    TokenListScope::Live
                },
            }),
        )
        .await?;
    let rows = reply
        .tokens
        .into_iter()
        .map(|token| TokenListView {
            token_id: token.token_id,
            created_at: token.created_at.to_string(),
            expires_at: token.expires_at.to_string(),
        })
        .collect::<Vec<_>>();
    Ok(render_token_list(&rows, OffsetDateTime::now_utc()))
}

async fn revoke(command: TokenRevokeCommand) -> Result<String, TokenExecutionError> {
    let remote = OperatorRemote::load(command.target.as_ref())?;
    let route = token_revoke_route(&command.token_id);
    let reply = remote
        .request_json_with_refusal::<_, TokenRevokeReply, TokenRevokeRefusal>(
            Method::POST,
            &route,
            Some(&TokenRevokeRequest {
                token_id: command.token_id,
            }),
        )
        .await?;
    match reply {
        JsonReply::Success(reply) => Ok(render_token_revoke(&reply.token_id)),
        JsonReply::Refused(TokenRevokeRefusal::NotFound { token_id }) => {
            Err(TokenExecutionError::TokenNotFound { token_id })
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TokenExecutionError {
    #[error(transparent)]
    Remote(#[from] OperatorRemoteError),
    #[error(
        "cannot create a join token because no public door endpoint is advertised; run `{repair_command}`"
    )]
    NoAdvertisedDoorEndpoint { repair_command: String },
    #[error(
        "cannot create a join token because the cluster advertises {found} door endpoints; this version supports at most {maximum}"
    )]
    TooManyAdvertisedDoorEndpoints { found: usize, maximum: usize },
    #[error("join token {token_id} does not exist or was already revoked")]
    TokenNotFound { token_id: TokenName },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenListView {
    pub token_id: TokenName,
    pub created_at: String,
    pub expires_at: String,
}

#[must_use]
pub fn render_token_create(
    token_id: &TokenName,
    blob: &str,
    expires_at: &str,
    ttl_seconds: u64,
) -> String {
    format!(
        "┌─ save this now — it is shown once ─────────────────────────┐\n\
JOIN_BLOB='{blob}'\n\
└────────────────────────────────────────────────────────────┘\n\
\n\
  join a machine:   sudo ployz machine join \"$JOIN_BLOB\"\n\
  cloud-init:       curl -fsSL https://ployz.sh | sh -s -- join \"$JOIN_BLOB\"\n\
\n\
  token  {}   expires {expires_at} ({})\n",
        token_id.as_str(),
        render_ttl_duration(ttl_seconds),
    )
}

#[must_use]
pub fn render_token_list(rows: &[TokenListView], now: OffsetDateTime) -> String {
    let mut rows = rows.iter().collect::<Vec<_>>();
    rows.sort_by(|left, right| left.token_id.as_str().cmp(right.token_id.as_str()));
    let mut output = String::from("ID\tEXPIRES\tCREATED\n");
    for row in rows {
        output.push_str(row.token_id.as_str());
        output.push('\t');
        output.push_str(&render_relative_timestamp(
            &row.expires_at,
            now,
            RelativeKind::Expiry,
        ));
        output.push('\t');
        output.push_str(&render_relative_timestamp(
            &row.created_at,
            now,
            RelativeKind::Age,
        ));
        output.push('\n');
    }
    output
}

#[must_use]
pub fn render_token_revoke(token_id: &TokenName) -> String {
    format!("revoked token {}\n", token_id.as_str())
}

#[derive(Debug, Clone, Copy)]
enum RelativeKind {
    Expiry,
    Age,
}

fn render_relative_timestamp(value: &str, now: OffsetDateTime, kind: RelativeKind) -> String {
    let Ok(instant) = OffsetDateTime::parse(value, &Rfc3339) else {
        return value.to_owned();
    };
    let seconds = match kind {
        RelativeKind::Expiry => instant.unix_timestamp() - now.unix_timestamp(),
        RelativeKind::Age => now.unix_timestamp() - instant.unix_timestamp(),
    };
    let magnitude = seconds.unsigned_abs();
    let duration = render_duration(magnitude.max(1));
    match (kind, seconds >= 0) {
        (RelativeKind::Expiry, true) => format!("in {duration}"),
        (RelativeKind::Expiry, false) => format!("expired {duration} ago"),
        (RelativeKind::Age, true) => format!("{duration} ago"),
        (RelativeKind::Age, false) => format!("in {duration}"),
    }
}

fn render_absolute_utc(value: &str) -> String {
    let Ok(instant) = OffsetDateTime::parse(value, &Rfc3339) else {
        return value.to_owned();
    };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02} UTC",
        instant.year(),
        u8::from(instant.month()),
        instant.day(),
        instant.hour(),
        instant.minute(),
    )
}

fn render_duration(seconds: u64) -> String {
    for (divisor, suffix) in [(86_400, "d"), (3_600, "h"), (60, "m")] {
        if seconds >= divisor && seconds.is_multiple_of(divisor) {
            return format!("{}{suffix}", seconds / divisor);
        }
    }
    format!("{seconds}s")
}

fn render_ttl_duration(seconds: u64) -> String {
    for (divisor, suffix) in [(3_600, "h"), (60, "m")] {
        if seconds >= divisor && seconds.is_multiple_of(divisor) {
            return format!("{}{suffix}", seconds / divisor);
        }
    }
    format!("{seconds}s")
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN_A: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    const TOKEN_B: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAW";

    fn token_id(value: &str) -> TokenName {
        TokenName::try_new(value).expect("token id")
    }

    #[test]
    fn create_is_show_once_and_follow_up_lines_reuse_the_variable() {
        let blob = "pzjoin_opaque-secret";
        let output = render_token_create(
            &token_id(TOKEN_A),
            blob,
            "2026-08-06 14:00 UTC",
            24 * 60 * 60,
        );
        assert_eq!(output.matches(blob).count(), 1);
        assert!(output.contains("JOIN_BLOB='pzjoin_opaque-secret'"));
        assert!(output.contains("sudo ployz machine join \"$JOIN_BLOB\""));
        assert!(output.contains("curl -fsSL https://ployz.sh | sh -s -- join \"$JOIN_BLOB\""));
        assert!(output.contains("expires 2026-08-06 14:00 UTC (24h)"));
    }

    #[test]
    fn list_is_stable_and_human_first_for_live_and_expired_rows() {
        let now = OffsetDateTime::parse("2026-08-05T12:00:00Z", &Rfc3339).expect("now");
        let rows = vec![
            TokenListView {
                token_id: token_id(TOKEN_B),
                created_at: "2026-08-04T12:00:00Z".to_owned(),
                expires_at: "2026-08-05T07:00:00Z".to_owned(),
            },
            TokenListView {
                token_id: token_id(TOKEN_A),
                created_at: "2026-08-05T10:00:00Z".to_owned(),
                expires_at: "2026-08-06T10:00:00Z".to_owned(),
            },
        ];
        assert_eq!(
            render_token_list(&rows, now),
            format!(
                "ID\tEXPIRES\tCREATED\n{TOKEN_A}\tin 22h\t2h ago\n{TOKEN_B}\texpired 5h ago\t1d ago\n"
            )
        );
    }

    #[test]
    fn revoke_is_terse() {
        assert_eq!(
            render_token_revoke(&token_id(TOKEN_A)),
            format!("revoked token {TOKEN_A}\n")
        );
    }
}
