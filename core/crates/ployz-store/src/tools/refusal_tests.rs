use std::path::Path;

use super::*;

fn store_error(code: RpcErrorCode, message: &str, details: Value) -> RpcError {
    RpcError {
        code,
        message: message.to_owned(),
        details,
        cause: Vec::new(),
    }
}

fn next_of(line: &str) -> Option<Call> {
    match cli_next(line) {
        Next::Call(call) => Some(call),
        Next::Fix(_) | Next::Drop => None,
    }
}

#[test]
fn sanitize_cuts_the_cli_clause() {
    let code = RpcErrorCode::NotFound;
    assert_eq!(
        sanitize("Service web not found; run `ployz domain add web`", &code),
        "Service web not found"
    );
    assert_eq!(
        sanitize("Pick an Environment (pass --env NAME)", &code),
        "Pick an Environment"
    );
    assert_eq!(sanitize("Nothing to cut.", &code), "Nothing to cut.");
    assert_eq!(
        sanitize("ployz env new staging", &code),
        "The Config Store refused this call (not_found)"
    );
}

#[test]
fn a_cli_line_becomes_its_tool_call() {
    assert_eq!(
        next_of("ployz domain add web --env staging --project shop"),
        Some(Call::new(
            "domain_add",
            json!({ "service": "web", "environment": "staging", "project": "shop" })
        ))
    );
    assert_eq!(
        next_of("ployz env pr acme/api --from ENV --project shop"),
        Some(Call::new(
            "environment_pr",
            json!({ "repository": "acme/api", "enabled": true, "project": "shop" })
        ))
    );
    assert_eq!(
        next_of("ployz github ls acme/api --project shop --env staging"),
        Some(Call::new("github_ls", json!({ "repository": "acme/api" })))
    );
    assert_eq!(
        next_of("ployz volume set data --shared-writes"),
        Some(Call::new(
            "volume_update",
            json!({ "volume": "data", "shared_writes": true })
        ))
    );
    assert_eq!(
        next_of("ployz diff --env staging"),
        Some(Call::new("review", json!({ "environment": "staging" })))
    );
}

#[test]
fn a_placeholder_is_never_an_argument() {
    assert_eq!(
        next_of("ployz domain add SERVICE --env staging"),
        Some(Call::new("domain_add", json!({ "environment": "staging" })))
    );
}

#[test]
fn a_cli_line_for_a_human_becomes_a_fix() {
    assert!(matches!(
        cli_next("ployz server add"),
        Next::Fix(Fix::ConnectServers)
    ));
    assert!(matches!(
        cli_next("ployz set web.registryCredential --secret"),
        Next::Fix(Fix::OpenDashboard {
            page: DashboardPage::Variables
        })
    ));
    assert!(matches!(cli_next("ployz rm everything"), Next::Drop));
}

#[test]
fn every_cli_line_the_store_names_has_a_row() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut lines = Vec::new();
    let mut dirs = vec![src.clone()];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                if path != src.join("tools") {
                    dirs.push(path);
                }
                continue;
            }
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if !name.ends_with(".rs") || name.ends_with("_tests.rs") {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            for (at, _) in text.match_indices("\"ployz ") {
                let rest = text.get(at + 1..).unwrap();
                let line = rest.split('"').next().unwrap();
                let words = line
                    .split_whitespace()
                    .skip(1)
                    .take_while(|word| !word.starts_with("--") && !word.contains('{'))
                    .collect::<Vec<_>>();
                if !words.is_empty() {
                    lines.push((name.clone(), line.to_owned(), words.join(" ")));
                }
            }
        }
    }
    assert!(lines.len() >= 16, "the scan found {} lines", lines.len());
    let missing = lines
        .iter()
        .filter(|(_, _, words)| cli_row(&words.split(' ').collect::<Vec<_>>()).is_none())
        .collect::<Vec<_>>();
    assert!(missing.is_empty(), "no CLI_NEXT row for {missing:?}");
}

#[test]
fn a_confirmation_retry_accepts_exactly_the_named_loss() {
    let call = Call::new("publish", json!({ "environment": "staging" }));
    let error = store_error(
        RpcErrorCode::ConfirmationRequired,
        "Publishing deletes data; pass --accept-volume-loss data",
        json!({ "accept": ["data"], "version": "v7" }),
    );
    let refusal = from_store(error, &call, Writes::None);
    assert_eq!(refusal.message, "Publishing deletes data");
    assert_eq!(
        refusal.retry,
        Call::new(
            "publish",
            json!({ "environment": "staging", "accept_volume_loss": ["data"], "version": "v7" })
        )
    );
}

#[test]
fn another_refusal_keeps_the_same_call_as_its_retry() {
    let call = Call::new("publish", json!({ "environment": "staging" }));
    let error = store_error(
        RpcErrorCode::Conflict,
        "The Environment moved",
        json!({ "next": [{ "tool": "environment_show", "input": { "environment": "staging" } }, { "tool": "volume_remove" }, "ployz server add"] }),
    );
    let refusal = from_store(error, &call, Writes::Committed);
    assert!(refusal.retry.is_same());
    assert_eq!(refusal.writes, Writes::Committed);
    assert_eq!(
        refusal.next,
        vec![Call::new(
            "environment_show",
            json!({ "environment": "staging" })
        )]
    );
    assert_eq!(refusal.fix(), Some(Fix::ConnectServers));
}

#[test]
fn a_domain_key_is_no_setting() {
    assert_eq!(not_setting("managedHostnames"), Some("domain_add"));
    assert_eq!(not_setting("volumes"), Some("volume_add"));
    assert_eq!(not_setting("image"), None);
    assert!(not_setting_sentence("domain_add").contains("domain_update"));
}
