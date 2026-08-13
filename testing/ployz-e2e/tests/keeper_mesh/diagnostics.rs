use super::{
    API_PORT, DindMachine, Docker, Ipv6Addr, MACHINE_A_NAME, MACHINE_B_NAME,
    corrosion_curl_command, machine_status_query, query_documents, wait_for_command,
};
use serde_json::{Value, json};

pub(super) async fn wait_for_absolute_handshake(
    docker: &Docker,
    machine: &DindMachine,
    observer_machine_id: &str,
    remote_machine_id: &str,
) -> Result<(), String> {
    wait_for_command(
        docker,
        machine,
        "absolute latest-handshake testimony in machine_status",
        corrosion_curl_command("v1/queries", &machine_status_query()),
        |outcome| {
            outcome.success()
                && query_documents(&outcome.stdout).iter().any(|document| {
                    document.get("machine_id").and_then(Value::as_str) == Some(observer_machine_id)
                        && has_absolute_handshake(document, remote_machine_id)
                })
        },
    )
    .await
}

pub(super) async fn wait_for_public_status_handshake(
    docker: &Docker,
    source: &DindMachine,
    destination: Ipv6Addr,
    remote_machine_name: &str,
) -> Result<(), String> {
    let url = format!("http://[{destination}]:{API_PORT}/status");
    wait_for_command(
        docker,
        source,
        "authenticated API /status handshake age over WireGuard ULA",
        vec![
            "curl".to_owned(),
            "--fail-with-body".to_owned(),
            "--silent".to_owned(),
            "--show-error".to_owned(),
            "--max-time".to_owned(),
            "3".to_owned(),
            "--noproxy".to_owned(),
            "*".to_owned(),
            url,
        ],
        |outcome| {
            outcome.success()
                && serde_json::from_str::<Value>(&outcome.stdout).is_ok_and(|status| {
                    public_status_has_handshake_age(&status, remote_machine_name)
                })
        },
    )
    .await
}

fn has_absolute_handshake(document: &Value, remote_machine_id: &str) -> bool {
    let Some(handshake) = document
        .get("wireguard_handshakes")
        .and_then(Value::as_object)
        .and_then(|handshakes| handshakes.get(remote_machine_id))
    else {
        return false;
    };
    handshake.get("state").and_then(Value::as_str) == Some("at")
        && handshake
            .get("unix_seconds")
            .and_then(Value::as_u64)
            .is_some_and(|unix_seconds| unix_seconds > 0)
}

fn public_status_has_handshake_age(status: &Value, remote_machine_name: &str) -> bool {
    status.get("barrier").and_then(Value::as_str) == Some("ready")
        && status
            .get("machines")
            .and_then(Value::as_array)
            .is_some_and(|machines| {
                machines.iter().any(|machine| {
                    machine.get("name").and_then(Value::as_str) == Some(remote_machine_name)
                        && machine.pointer("/handshake/state").and_then(Value::as_str)
                            == Some("ago")
                        && machine
                            .pointer("/handshake/seconds")
                            .and_then(Value::as_u64)
                            .is_some()
                })
            })
}

#[test]
fn absolute_machine_handshake_testimony_is_projected_as_a_public_age() {
    let machine_status = json!({
        "machine_id": MACHINE_B_NAME,
        "wireguard_handshakes": {
            MACHINE_A_NAME: {"state": "at", "unix_seconds": 1_775_000_000_u64}
        }
    });
    let status = json!({
        "barrier": "ready",
        "machines": [
            {
                "name": "edge-a",
                "address": "fd00::1",
                "handshake": {"state": "ago", "seconds": 4, "freshness": "healthy"}
            },
            {
                "name": "edge-b",
                "address": "fd00::2",
                "handshake": {"state": "self_machine"}
            }
        ]
    });

    assert!(has_absolute_handshake(&machine_status, MACHINE_A_NAME));
    assert!(public_status_has_handshake_age(&status, MACHINE_A_NAME));
    assert!(!public_status_has_handshake_age(
        &json!({
            "barrier": "ready",
            "machines": [{
                "name": MACHINE_A_NAME,
                "handshake": {"state": "never"}
            }]
        }),
        MACHINE_A_NAME,
    ));
}
