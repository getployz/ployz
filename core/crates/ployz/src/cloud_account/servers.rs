//! The Servers of the credential's Organization as Cloud sees them: the Management
//! Capability it holds on each, what forgetting them lets go of, and Cloud's removal of
//! one.

use std::{collections::BTreeMap, time::Duration};

use ployz_core::{Departed, MachineId, MachineName, RpcError};
use reqwest::{Method, StatusCode};
use serde::{Deserialize, Serialize};

use super::{
    calls::{approved_answer, follow_run, refusable_operation, start_run},
    credential::Credential,
    http::{self, Effect},
    reply::{StoreCallError, answer, refusal},
};

/// This credential's own Management Capability on each Server Cloud could reach.
#[derive(Debug, Deserialize)]
pub(crate) struct ServerAccess {
    pub(crate) connections: Vec<crate::context::Connection>,
    /// Servers that didn't answer Cloud's provisioning of a holder now.
    pub(crate) unreachable: Vec<MachineId>,
    /// Servers Cloud couldn't provision a holder on for another reason, such as their own refusal.
    #[serde(default)]
    pub(crate) refused: Vec<RefusedServer>,
    /// The names enrollment gave the Organization's Servers, where Cloud holds one.
    #[serde(default, deserialize_with = "text_names")]
    pub(crate) names: BTreeMap<MachineId, String>,
}

/// A Server that gave Cloud no holder for a reason other than silence, with that reason.
#[derive(Debug, Deserialize)]
pub(crate) struct RefusedServer {
    pub(crate) machine_id: MachineId,
    pub(crate) message: String,
}

/// Names are display only: an entry that is not a Machine ID and a string is dropped, so
/// that Server shows by its id and the reply's connections still dial.
fn text_names<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeMap<MachineId, String>, D::Error> {
    let serde_json::Value::Object(names) = serde_json::Value::deserialize(deserializer)? else {
        return Ok(BTreeMap::new());
    };
    Ok(names
        .into_iter()
        .filter_map(|(id, name)| Some((MachineId::parse(id).ok()?, name.as_str()?.to_owned())))
        .collect())
}

/// This credential's own Management Capability on each Server of its Organization; Cloud
/// provisions a missing one, including on Servers enrolled after login.
///
/// # Errors
///
/// Returns Cloud's refusal when the credential's user is not an owner or admin, or a
/// Cloud failure.
pub(crate) async fn server_access(credential: &Credential) -> Result<ServerAccess, StoreCallError> {
    let url = format!("{}/api/cli/server-access", credential.cloud());
    let request = http::request(credential, Method::POST, &url, None)?;
    let reply = http::exchange(credential.cloud(), request, Effect::Read).await?;
    if reply.status == StatusCode::FORBIDDEN
        && let Some(refused) = refusal(&reply.body)
    {
        return Err(StoreCallError::Refused(refused));
    }
    Ok(answer(credential, reply.status, &reply.body)?)
}

/// Why a Server didn't count as reachable when Cloud tried it. One that answered
/// refuses the whole forget, so it never appears here.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Reach {
    /// Cloud tried and it didn't answer in time.
    DidntAnswer,
    /// Cloud holds no connection to try, such as a founder that never published one.
    NoConnection,
}

/// One Server of the Organization as Cloud found it when it tried. Its name is the
/// one enrollment assigned, else its ID.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ObservedServer {
    pub(crate) id: MachineId,
    pub(crate) name: String,
    pub(crate) reach: Reach,
}

/// What forgetting the Organization's Servers lets go of: each Server with what Cloud
/// observed of it, and each Volume a Deploy put on them.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ForgetCheck {
    pub(crate) organization: String,
    pub(crate) servers: Vec<ObservedServer>,
    pub(crate) volumes: Vec<ployz_store::AppliedVolume>,
}

/// What forgetting the Organization's Servers let go of, with the Deployments that
/// might still have run, cancelled.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ServersForgotten {
    #[serde(flatten)]
    pub(crate) check: ForgetCheck,
    pub(crate) cancelled: Vec<ployz_store::DeploymentId>,
}

/// What forgetting the credential's Organization's Servers would let go of. Cloud
/// tries each Server first.
///
/// # Errors
///
/// Returns Cloud's refusal (`conflict` while a Server answers or one is still
/// joining, `forbidden` for a member who isn't an owner or admin), or a Cloud failure.
pub(crate) async fn check_forget_servers(
    credential: &Credential,
) -> Result<ForgetCheck, StoreCallError> {
    refusable_operation(credential, Method::GET, "forget-servers", None).await
}

/// Forget the Servers of the credential's Organization, named `organization`: Cloud
/// tries each again and forgets them only when none answers and nothing changed.
///
/// # Errors
///
/// As [`check_forget_servers`], `invalid_argument` for another Organization than the
/// credential's, and `approval_required` until a human approves it.
pub(crate) async fn forget_servers(
    credential: &Credential,
    organization: &str,
    approval: Option<&str>,
) -> Result<ServersForgotten, StoreCallError> {
    let body = serde_json::json!({ "organization": organization });
    approved_answer(credential, Method::POST, "forget-servers", &body, approval).await
}

/// What Cloud's removal of a Server did.
#[derive(Debug, Deserialize)]
pub(crate) struct CloudRemoval {
    /// `departed` as prose, and all an older Cloud says.
    pub(crate) reset_warning: Option<String>,
    /// How the Server left. None from an older Cloud, for an attempt row older than the tag, for
    /// a removal without a reset, or for a kind of departure newer than this CLI.
    #[serde(default, deserialize_with = "known_departed")]
    pub(crate) departed: Option<Departed>,
    pub(crate) release: Release,
}

/// A departure this CLI knows, or none: a newer Cloud may send a kind it doesn't.
fn known_departed<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Departed>, D::Error> {
    Ok(Option::<serde_json::Value>::deserialize(deserializer)?
        .and_then(|departed| serde_json::from_value(departed).ok()))
}

/// What became of Cloud's hold once the Server was gone.
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum Release {
    /// Other Servers keep the Cluster; Cloud dropped only this one.
    OthersRemain,
    /// It was the last: Cloud forgot the Cluster, and the next Server founds a new one.
    Released,
    /// Cloud kept its hold, for `reason`.
    Kept { reason: String },
}

#[derive(Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum RemovalRun {
    Succeeded(CloudRemoval),
}

/// How long `server rm` and `server forget` wait on Cloud's removal before it stops watching.
/// It outlasts the ~11 minutes Cloud spends retrying a leaving Server that reads up but
/// doesn't answer before it goes ahead.
const REMOVAL_WAIT: std::time::Duration = std::time::Duration::from_secs(20 * 60);

/// Have Cloud remove Server `machine` of the credential's Organization, resetting it
/// and accepting exactly `confirmation`'s Data Loss. It is the same durable removal the
/// dashboard starts, which Cloud runs under its own connection; [`follow_removal`]
/// waits for it. The run keeps `machine`'s name, the one the CLI showed. `approval` is
/// the id of the human's approval, when Cloud asked for one.
///
/// # Errors
///
/// Returns Cloud's refusal (`not_found` for a Server it doesn't reach, `invalid_argument`
/// when the confirmation misses fresh Data Loss, `approval_required`), or a Cloud failure.
pub(crate) async fn start_removal(
    credential: &Credential,
    machine: &ployz_core::Machine,
    confirmation: &ployz_core::DataLossConfirmation,
    approval: Option<&str>,
) -> Result<String, StoreCallError> {
    start_run(
        credential,
        Method::DELETE,
        &format!("servers/{}", machine.id),
        &serde_json::json!({ "confirm_data_loss": confirmation, "server_name": machine.name }),
        approval,
    )
    .await
}

/// Have Cloud forget Server `machine` of the credential's Organization: take it out of
/// the Cluster without a reset. Cloud runs it as the same durable removal
/// [`follow_removal`] waits for, and it keeps `machine`'s name, the one the CLI
/// showed. `approval` is the id of the human's approval, when Cloud asked for one.
///
/// # Errors
///
/// Returns Cloud's refusal (`not_found` for a Server it doesn't reach,
/// `approval_required`), or a Cloud failure.
pub(crate) async fn start_forget(
    credential: &Credential,
    machine: &ployz_core::Machine,
    approval: Option<&str>,
) -> Result<String, StoreCallError> {
    start_run(
        credential,
        Method::POST,
        &format!("servers/{}/forget", machine.id),
        &serde_json::json!({ "server_name": machine.name }),
        approval,
    )
    .await
}

/// Wait for Cloud's removal `id` of Server `name` to settle. Cloud drops its row for
/// the Server, and when it saw the last one reset, lets go of the Cluster.
///
/// # Errors
///
/// Returns Cloud's refusal (`unavailable` when the removal failed), `unavailable` when it
/// hasn't settled within [`REMOVAL_WAIT`], or a Cloud failure.
pub(crate) async fn follow_removal(
    credential: &Credential,
    name: &MachineName,
    verb: &str,
    id: &str,
) -> Result<CloudRemoval, StoreCallError> {
    let deadline = tokio::time::Instant::now() + REMOVAL_WAIT;
    let doing = format!("Removing Server {name}");
    let path = format!("server-removals/{id}");
    let followed = tokio::select! {
        followed = follow_run(credential, &path, &doing, Some(deadline)) => followed,
        () = removal_progress(verb, name, crate::ui::note) => unreachable!("progress never ends"),
    };
    match followed {
        Ok(Some(RemovalRun::Succeeded(removal))) => Ok(removal),
        Ok(None) => Err(StoreCallError::Refused(removal_unsettled(name))),
        Err(StoreCallError::Refused(refused))
            if refused.details.get("unfinished") == Some(&serde_json::Value::Bool(true)) =>
        {
            // Any step after the reset was sent may be the one that ran out: the Server may
            // have reset and Cloud dropped it, or neither.
            Err(StoreCallError::Refused(
                RpcError::unavailable(format!(
                    "Cloud's removal of Server {name} didn't finish: it ran out of retries. If \
                     the Server is gone but `ployz server ls` still lists it, forget it."
                ))
                .with_details(serde_json::json!({ "next": "ployz server ls" }))
                .with_causes(vec![refused.message]),
            ))
        }
        Err(error) => Err(error),
    }
}

/// What `server rm` or `server forget` says on stderr while Cloud's removal of Server `name`
/// runs, `verb` being "removing" or "forgetting": why it can take long after 30 seconds, then
/// the minutes so far once a minute. It never ends; the follow it runs beside does.
async fn removal_progress(verb: &str, name: &MachineName, mut say: impl FnMut(String)) {
    let started = tokio::time::Instant::now();
    tokio::time::sleep_until(started + Duration::from_secs(30)).await;
    say(format!(
        "Ployz Cloud is still {verb} Server {name}. A Server that is up but doesn't answer takes \
         it up to about 12 minutes, since it retries the Server first in case it still writes a \
         Volume."
    ));
    for minutes in 1u64.. {
        tokio::time::sleep_until(started + Duration::from_secs(minutes * 60)).await;
        say(format!("Still {verb} Server {name} ({minutes} min)…"));
    }
}

/// Stopped watching a removal that is still running. It may yet fail unfinished, as
/// [`follow_removal`] says when it does. No `next`: nothing is to run while it runs.
fn removal_unsettled(name: &MachineName) -> RpcError {
    RpcError::unavailable(format!(
        "Cloud is still removing Server {name}; it finishes on its own. \
             The dashboard's Servers page shows when it's done. If it fails and the Server \
             is gone but `ployz server ls` still lists it, forget it."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud_account::testing::{fake_cloud, token};

    #[test]
    fn a_removal_reads_its_departure_and_an_older_or_newer_cloud_reads_without_one() {
        let departed = |body: &str| serde_json::from_str::<CloudRemoval>(body).unwrap().departed;
        let release = r#""release":{"kind":"others_remain"}"#;
        assert_eq!(
            departed(&format!(
                r#"{{"reset_warning":"replicated delete failed","departed":{{"kind":"reset","row_warning":"replicated delete failed"}},{release}}}"#
            )),
            Some(Departed::Reset {
                row_warning: Some("replicated delete failed".into())
            })
        );
        assert_eq!(
            departed(&format!(r#"{{"reset_warning":null,{release}}}"#)),
            None,
            "an older Cloud"
        );
        assert_eq!(
            departed(&format!(
                r#"{{"reset_warning":null,"departed":null,{release}}}"#
            )),
            None,
            "a row older than the tag"
        );
        assert_eq!(
            departed(&format!(
                r#"{{"reset_warning":"moved","departed":{{"kind":"moved"}},{release}}}"#
            )),
            None,
            "a newer Cloud"
        );
    }

    #[test]
    fn a_removal_the_cli_stopped_waiting_on_says_when_to_forget_but_not_to_now() {
        let name = MachineName::parse("web-2").unwrap();
        let unsettled = removal_unsettled(&name);
        assert!(
            unsettled
                .message
                .contains("Cloud is still removing Server web-2"),
            "{}",
            unsettled.message
        );
        // A Server Cloud already dropped isn't found by forget, so only a listed one is sent there.
        assert!(
            unsettled
                .message
                .contains("is gone but `ployz server ls` still lists it"),
            "{}",
            unsettled.message
        );
        // The removal still runs, so forgetting now would only find it in progress.
        assert_eq!(unsettled.details.get("next"), None);
    }

    #[tokio::test(start_paused = true)]
    async fn a_removal_says_why_it_waits_after_30_seconds_then_once_a_minute() {
        let name = MachineName::parse("web-2").unwrap();
        let lines = std::cell::RefCell::new(Vec::new());
        let progress = removal_progress("forgetting", &name, |line| lines.borrow_mut().push(line));
        tokio::pin!(progress);
        let at = |seconds| tokio::time::sleep(Duration::from_secs(seconds));

        tokio::select! { biased; () = &mut progress => unreachable!(), () = at(29) => {} }
        assert!(lines.borrow().is_empty(), "{:?}", lines.borrow());
        tokio::select! { biased; () = &mut progress => unreachable!(), () = at(1) => {} }
        let why = lines.borrow().clone();
        assert!(
            matches!(&why[..], [line] if line.starts_with("Ployz Cloud is still forgetting Server web-2. ")),
            "{why:?}"
        );
        tokio::select! { biased; () = &mut progress => unreachable!(), () = at(150) => {} }
        assert_eq!(
            lines.borrow().get(1..).unwrap_or_default(),
            [
                "Still forgetting Server web-2 (1 min)…",
                "Still forgetting Server web-2 (2 min)…",
                "Still forgetting Server web-2 (3 min)…",
            ]
        );
    }

    #[test]
    fn a_name_that_is_not_text_costs_only_that_name() {
        let (held, odd, gone) = ("a".repeat(32), "b".repeat(32), "c".repeat(32));
        let access: ServerAccess = serde_json::from_value(serde_json::json!({
            "connections": [{ "machine_id": held, "unix": "/run/ployz.sock" }],
            "unreachable": [odd, gone],
            "names": { held.clone(): "web-1", odd: null, gone: 7, "not-an-id": "web-9" },
        }))
        .unwrap();

        assert_eq!(access.connections.len(), 1);
        assert_eq!(access.unreachable.len(), 2);
        assert_eq!(
            access.names,
            BTreeMap::from([(MachineId::parse(held).unwrap(), "web-1".to_owned())])
        );
    }

    #[test]
    fn names_that_are_not_an_object_leave_every_server_by_id() {
        let access: ServerAccess = serde_json::from_value(serde_json::json!({
            "connections": [],
            "unreachable": [],
            "names": null,
        }))
        .unwrap();

        assert!(access.names.is_empty());
    }

    #[tokio::test]
    async fn a_forget_check_waits_past_the_cluster_time_cloud_may_spend_on_it() {
        // Cloud tries every Server at once: a connect (10s), then its name (2s).
        let answers_after = Duration::from_secs(12) + Duration::from_secs(5);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let cloud = format!("http://{}", listener.local_addr().unwrap());
        let id = "a".repeat(32);
        let reply = serde_json::json!({
            "organization": "acme",
            "servers": [{ "id": id, "name": "web-1", "reach": "didnt_answer" }],
            "volumes": [],
        })
        .to_string();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = vec![0; 8192];
            let read = tokio::io::AsyncReadExt::read(&mut stream, &mut request)
                .await
                .unwrap();
            assert!(
                request
                    .get(..read)
                    .unwrap()
                    .starts_with(b"GET /api/cli/forget-servers ")
            );
            tokio::time::pause();
            tokio::time::sleep(answers_after).await;
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}",
                reply.len()
            );
            tokio::io::AsyncWriteExt::write_all(&mut stream, response.as_bytes())
                .await
                .unwrap();
        });

        let check = check_forget_servers(&token(&cloud)).await.unwrap();
        assert_eq!(check.organization, "acme");
        let [server] = check.servers.as_slice() else {
            panic!("{:?}", check.servers)
        };
        assert_eq!(server.id, MachineId::parse(id).unwrap());
        assert_eq!(server.reach, Reach::DidntAnswer);
        assert!(check.volumes.is_empty());
    }

    #[tokio::test]
    async fn server_access_reads_dialable_connections() {
        let capability = format!("ployz1:{}", "A".repeat(86));
        let reply = serde_json::json!({
            "connections": [{ "machine_id": "a".repeat(32), "management": capability }],
            "unreachable": ["b".repeat(32)],
        });
        let cloud = fake_cloud(move |route, _| match route {
            "POST /api/cli/server-access" => (200, reply.clone()),
            other => panic!("unexpected {other}"),
        });
        let access = server_access(&token(&cloud)).await.unwrap();
        assert_eq!(access.connections.len(), 1);
        assert_eq!(
            access.unreachable,
            [MachineId::parse("b".repeat(32)).unwrap()]
        );
        assert!(!format!("{access:?}").contains(&capability));
    }
}
