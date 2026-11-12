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
        next_of("ployz project new NAME"),
        Some(Call::new("project_create", json!({})))
    );
    assert_eq!(
        next_of("ployz diff --env staging"),
        Some(Call::new("review", json!({ "environment": "staging" })))
    );
}

/// What an agent is told of each `details.next` the Store's producers write, as the
/// Store wrote them before follow-ups were typed: the next call or the fix, or neither.
#[test]
fn every_store_follow_up_reads_as_main_wrote_it() {
    let scope = json!({ "project": "shop", "environment": "staging" });
    let call = |tool: &str, input: Value| (Some(Call::new(tool, input)), None);
    let fix = |page: DashboardPage| (None, Some(Fix::OpenDashboard { page }));
    let nothing = (None, None);
    let shown = |environment: &str| {
        call(
            "environment_show",
            json!({ "project": "shop", "environment": environment }),
        )
    };
    type Told = (Option<Call>, Option<Fix>);
    let cases: Vec<(Value, Told)> = vec![
        (json!("ployz server add"), (None, Some(Fix::ConnectServers))),
        (
            json!("ployz server clean --namespace shop-staging --confirm shop-staging"),
            fix(DashboardPage::Servers),
        ),
        (
            json!("ployz set web.registryCredential --secret"),
            fix(DashboardPage::Variables),
        ),
        (
            json!("ployz set web.updateOrder=stop-first --env staging --project shop"),
            fix(DashboardPage::Variables),
        ),
        (
            json!(
                "ployz set web.env.KEY --rename NEW --update-references --env staging --project shop"
            ),
            fix(DashboardPage::Variables),
        ),
        (
            json!(
                "ployz set web --patch '{\"healthcheck\":{\"command\":\"pg_isready\"}}' --env staging --project shop"
            ),
            fix(DashboardPage::Variables),
        ),
        (
            json!(
                "sync without --value first, then ployz set web.env.KEY --secret --env staging --project shop"
            ),
            nothing.clone(),
        ),
        (
            json!("ployz project new NAME"),
            call("project_create", json!({})),
        ),
        (
            json!("ployz project ls"),
            call("environment_show", json!({})),
        ),
        (
            json!("ployz env new fix --project shop"),
            fix(DashboardPage::Environments),
        ),
        (
            json!("ployz env branch NAME --from staging --project shop"),
            fix(DashboardPage::Environments),
        ),
        (
            json!("ployz env sync --to ENV --project shop --env staging"),
            fix(DashboardPage::Environments),
        ),
        (
            json!("ployz env setup --clear --env staging --project shop"),
            fix(DashboardPage::Environments),
        ),
        (
            json!("ployz env setup --setup SERVICE=COMMAND --env staging --project shop"),
            fix(DashboardPage::Environments),
        ),
        (
            json!("ployz env pr acme/api --from ENV --project shop"),
            call(
                "environment_pr",
                json!({ "repository": "acme/api", "enabled": true, "project": "shop" }),
            ),
        ),
        (
            json!("ployz github ls acme/api"),
            call("github_ls", json!({ "repository": "acme/api" })),
        ),
        (
            json!("ployz github connect"),
            call("github_connect", json!({})),
        ),
        (json!("ployz deploy"), call("deploy", json!({}))),
        (json!("ployz deploy --no-prune"), call("deploy", json!({}))),
        (
            json!("ployz deployment ls --project shop --env staging"),
            call("deployment_list", scope.clone()),
        ),
        (
            json!("ployz diff --project shop --env staging"),
            call("review", scope.clone()),
        ),
        (
            json!("ployz domain add web"),
            call("domain_add", json!({ "service": "web" })),
        ),
        (
            json!(
                "ployz domain rm a --project shop --env staging && ployz domain rm b --project shop --env staging"
            ),
            nothing.clone(),
        ),
        (
            json!("ployz service rename setup NAME --env staging --project shop"),
            call(
                "service_rename",
                json!({ "service": "setup", "environment": "staging", "project": "shop" }),
            ),
        ),
        (
            json!("ployz volume inspect data --env staging --project shop"),
            call("environment_show", scope.clone()),
        ),
        (
            json!("ployz volume set data --shared-writes --env staging --project shop"),
            call(
                "volume_update",
                json!({ "volume": "data", "shared_writes": true, "environment": "staging", "project": "shop" }),
            ),
        ),
        (
            json!(
                "ployz volume add data --mount web:/data --docker --shared-writes --env staging --project shop"
            ),
            nothing.clone(),
        ),
        (json!("curl -fsSL https://ployz.sh | sh"), nothing.clone()),
        (
            json!("curl -fsSL https://ployz.sh | sh -s -- beta"),
            nothing.clone(),
        ),
        (shown_object("environment_show", &scope), shown("staging")),
        (
            json!({ "tool": "project_show", "input": { "project": "shop" } }),
            call("environment_show", json!({ "project": "shop" })),
        ),
        (
            json!({ "tool": "environment_protect", "input": { "project": "shop", "environment": "staging", "protected": false } }),
            fix(DashboardPage::Environments),
        ),
        (
            json!({ "tool": "remove", "input": { "project": "shop", "environment": "staging" } }),
            nothing.clone(),
        ),
        (
            json!({ "tool": "keep_branch", "input": { "environment": { "project": "shop", "environment": "fix" }, "kept": false } }),
            nothing,
        ),
    ];
    for (next, (call, fix)) in cases {
        let error = store_error(
            RpcErrorCode::Conflict,
            "Refused",
            json!({ "next": next.clone() }),
        );
        let refusal = from_store(error, &Call::new("deploy", json!({})), Writes::None);
        assert_eq!(refusal.next, call.into_iter().collect::<Vec<_>>(), "{next}");
        assert_eq!(refusal.fix(), fix, "{next}");
    }
}

fn shown_object(tool: &str, input: &Value) -> Value {
    json!({ "tool": tool, "input": input })
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
    assert!(matches!(
        cli_next("ployz volume add data --mount web:/data --docker --shared-writes"),
        Next::Drop
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
    let call = Call::new(
        "deploy",
        json!({ "environment": "staging", "skip_deploy": true }),
    );
    let error = store_error(
        RpcErrorCode::ConfirmationRequired,
        "Next deploy deletes volume data; pass --accept-volume-loss data",
        json!({ "accept": ["data"], "version": "v7" }),
    );
    let refusal = from_store(error, &call, Writes::None);
    assert_eq!(refusal.message, "Next deploy deletes volume data");
    assert_eq!(
        refusal.retry,
        Call::new(
            "deploy",
            json!({ "environment": "staging", "skip_deploy": true, "accept_volume_loss": ["data"], "version": "v7" })
        )
    );
}

#[test]
fn another_refusal_keeps_the_same_call_as_its_retry() {
    let call = Call::new(
        "deploy",
        json!({ "environment": "staging", "skip_deploy": true }),
    );
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
fn a_claim_refusal_carries_the_claim_url() {
    let call = Call::new(
        "domain_add",
        json!({ "service": "web", "hostname": "app.example.com" }),
    );
    let error = store_error(
        RpcErrorCode::from("claim_required"),
        "Claim this Organization to add a custom domain.",
        json!({
            "claim_url": "https://ployz.app/claim/abc",
            "action": "add_domain",
            "next": "Open https://ployz.app/claim/abc and sign in with GitHub, then try again.",
        }),
    );
    let refusal = from_store(error, &call, Writes::None);
    assert_eq!(refusal.code, RpcErrorCode::from("claim_required"));
    assert_eq!(
        refusal.fix(),
        Some(Fix::Claim {
            url: "https://ployz.app/claim/abc".into()
        })
    );
    assert!(
        !refusal.retries(),
        "the same call fails until a person claims"
    );
}

#[test]
fn a_domain_key_is_no_setting() {
    assert_eq!(
        not_setting("managedHostnames").map(ToolId::name),
        Some("domain_add")
    );
    assert_eq!(not_setting("volumes").map(ToolId::name), Some("volume_add"));
    assert_eq!(not_setting("image"), None);
    assert!(not_setting_sentence(ToolId::DomainAdd).contains("domain_update"));
}

#[test]
fn an_unnamed_project_refusal_names_the_field_and_each_project() {
    let refusal = from_store(
        store_error(
            RpcErrorCode::Ambiguous,
            "Name a Project: this Organization has more than one",
            json!({ "projects": ["eval-exit", "eval-oom"] }),
        ),
        &Call::new("deployment_show", json!({ "environment": "production" })),
        Writes::None,
    );
    assert_eq!(refusal.field.as_deref(), Some("project"));
    assert_eq!(refusal.valid, ["eval-exit", "eval-oom"]);
}

// #3473: a retry built for the call, as a confirmation's is, still never repeats a
// secret's text the call sent.
#[test]
fn a_built_retry_never_repeats_a_secret_s_text() {
    let call = Call::new(
        "service_update",
        json!({ "service": "db", "set": { "env": { "API_KEY": { "secret": "SENTINEL-3473" } } } }),
    );
    let error = store_error(
        RpcErrorCode::ConfirmationRequired,
        "This drops a volume",
        json!({ "accept": ["data"], "version": "v1" }),
    );
    let refusal = from_store(error, &call, Writes::None);
    assert_eq!(refusal.retry.tool, "service_update");
    let text = serde_json::to_string(&refusal).unwrap();
    assert!(!text.contains("SENTINEL-3473"), "{text}");
    assert!(json!(refusal).get("retry").is_none(), "{text}");

    let plain = Call::new("service_update", json!({ "service": "db" }));
    let refusal = Refusal::conflict("Busy").with_retry(plain.clone());
    assert_eq!(
        json!(refusal).get("retry"),
        Some(&json!(plain)),
        "a retry without a secret stays"
    );
}
