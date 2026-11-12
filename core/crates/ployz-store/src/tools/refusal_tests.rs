use super::*;
use crate::followup::{SetupLeft, Storage};
use crate::{EnvironmentName, ProjectName};

fn store_error(code: RpcErrorCode, message: &str, details: Value) -> RpcError {
    RpcError {
        code,
        message: message.to_owned(),
        details,
        cause: Vec::new(),
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

/// Each follow-up a Store producer writes: the `details.next` it wrote before
/// follow-ups were typed, and what an agent was told of it, which is unchanged.
#[test]
fn every_store_follow_up_reads_as_main_wrote_it() {
    let project = || ProjectName::parse("shop").unwrap();
    let environment = |name: &str| EnvironmentName::parse(name).unwrap();
    let at = |name: &str| Place {
        project: project(),
        environment: environment(name),
    };
    let staging = || at("staging");
    let scope = json!({ "project": "shop", "environment": "staging" });
    let call = |tool: &str, input: Value| (Some(Call::new(tool, input)), None);
    let fix = |page: DashboardPage| (None, Some(Fix::OpenDashboard { page }));
    let nothing = (None, None);
    type Told = (Option<Call>, Option<Fix>);
    let cases: Vec<(FollowUp, Value, Told)> = vec![
        (
            FollowUp::ServerAdd,
            json!("ployz server add"),
            (None, Some(Fix::ConnectServers)),
        ),
        (
            FollowUp::ServerClean {
                namespace: "shop-staging".into(),
            },
            json!("ployz server clean --namespace shop-staging --confirm shop-staging"),
            fix(DashboardPage::Servers),
        ),
        (
            FollowUp::SetRegistryCredential {
                service: "web".into(),
            },
            json!("ployz set web.registryCredential --secret"),
            fix(DashboardPage::Variables),
        ),
        (
            FollowUp::SetStopFirst {
                service: "web".into(),
                place: staging(),
            },
            json!("ployz set web.updateOrder=stop-first --env staging --project shop"),
            fix(DashboardPage::Variables),
        ),
        (
            FollowUp::RenameVariable {
                service: "web".into(),
                key: "KEY".into(),
                to: "NEW".into(),
                place: staging(),
            },
            json!(
                "ployz set web.env.KEY --rename NEW --update-references --env staging --project shop"
            ),
            fix(DashboardPage::Variables),
        ),
        (
            FollowUp::PatchHealthcheck {
                service: "web".into(),
                place: staging(),
            },
            json!(
                "ployz set web --patch '{\"healthcheck\":{\"command\":\"pg_isready\"}}' --env staging --project shop"
            ),
            fix(DashboardPage::Variables),
        ),
        (
            FollowUp::SyncThenSetSecret {
                label: "web.env.KEY".into(),
                place: staging(),
            },
            json!(
                "sync without --value first, then ployz set web.env.KEY --secret --env staging --project shop"
            ),
            nothing.clone(),
        ),
        (
            FollowUp::ProjectNew,
            json!("ployz project new NAME"),
            call("project_create", json!({})),
        ),
        (
            FollowUp::ProjectList,
            json!("ployz project ls"),
            call("environment_show", json!({})),
        ),
        (
            FollowUp::EnvironmentList { project: project() },
            json!("ployz env ls --project shop"),
            call("environment_show", json!({ "project": "shop" })),
        ),
        (
            FollowUp::EnvironmentNew {
                name: environment("fix"),
                project: project(),
            },
            json!("ployz env new fix --project shop"),
            fix(DashboardPage::Environments),
        ),
        (
            FollowUp::EnvironmentBranch {
                from: environment("staging"),
                project: project(),
            },
            json!("ployz env branch NAME --from staging --project shop"),
            fix(DashboardPage::Environments),
        ),
        (
            FollowUp::EnvironmentSync { place: staging() },
            json!("ployz env sync --to ENV --project shop --env staging"),
            fix(DashboardPage::Environments),
        ),
        (
            FollowUp::EnvironmentSetup {
                place: staging(),
                left: SetupLeft::One,
            },
            json!("ployz env setup --clear --env staging --project shop"),
            fix(DashboardPage::Environments),
        ),
        (
            FollowUp::EnvironmentSetup {
                place: staging(),
                left: SetupLeft::Several,
            },
            json!("ployz env setup --setup SERVICE=COMMAND --env staging --project shop"),
            fix(DashboardPage::Environments),
        ),
        (
            FollowUp::EnvironmentPr {
                repository: "acme/api".into(),
                project: project(),
            },
            json!("ployz env pr acme/api --from ENV --project shop"),
            call(
                "environment_pr",
                json!({ "repository": "acme/api", "enabled": true, "project": "shop" }),
            ),
        ),
        (
            FollowUp::GithubList {
                repository: "acme/api".into(),
            },
            json!("ployz github ls acme/api"),
            call("github_ls", json!({ "repository": "acme/api" })),
        ),
        (
            FollowUp::GithubConnect,
            json!("ployz github connect"),
            call("github_connect", json!({})),
        ),
        (
            FollowUp::Deploy { no_prune: false },
            json!("ployz deploy"),
            call("deploy", json!({})),
        ),
        (
            FollowUp::Deploy { no_prune: true },
            json!("ployz deploy --no-prune"),
            call("deploy", json!({})),
        ),
        (
            FollowUp::DeploymentList { place: staging() },
            json!("ployz deployment ls --project shop --env staging"),
            call("deployment_list", scope.clone()),
        ),
        (
            FollowUp::Diff { place: staging() },
            json!("ployz diff --project shop --env staging"),
            call("review", scope.clone()),
        ),
        (
            FollowUp::DomainAdd {
                service: "web".into(),
            },
            json!("ployz domain add web"),
            call("domain_add", json!({ "service": "web" })),
        ),
        (
            FollowUp::DomainRemove {
                prefixes: vec!["a".into(), "b".into()],
                place: staging(),
            },
            json!(
                "ployz domain rm a --project shop --env staging && ployz domain rm b --project shop --env staging"
            ),
            nothing.clone(),
        ),
        (
            FollowUp::RenameSetupService { place: staging() },
            json!("ployz service rename setup NAME --env staging --project shop"),
            call(
                "service_rename",
                json!({ "service": "setup", "environment": "staging", "project": "shop" }),
            ),
        ),
        (
            FollowUp::VolumeInspect {
                volume: "data".into(),
                place: staging(),
            },
            json!("ployz volume inspect data --env staging --project shop"),
            call("environment_show", scope.clone()),
        ),
        (
            FollowUp::VolumeAllowSharedWrites {
                volume: "data".into(),
                place: staging(),
            },
            json!("ployz volume set data --shared-writes --env staging --project shop"),
            call(
                "volume_update",
                json!({ "volume": "data", "shared_writes": true, "environment": "staging", "project": "shop" }),
            ),
        ),
        (
            FollowUp::VolumeAdd {
                volume: "data".into(),
                mounts: vec!["web:/data".into()],
                storage: Storage::Docker,
                place: staging(),
            },
            json!(
                "ployz volume add data --mount web:/data --docker --shared-writes --env staging --project shop"
            ),
            nothing.clone(),
        ),
        (
            FollowUp::VolumeAdd {
                volume: "data".into(),
                mounts: vec!["web:/data".into(), "worker:/my data".into()],
                storage: Storage::Size("10GiB".into()),
                place: staging(),
            },
            json!(
                "ployz volume add data --mount web:/data --mount 'worker:/my data' --size 10GiB --shared-writes --env staging --project shop"
            ),
            nothing.clone(),
        ),
        (
            FollowUp::UpdateClient { beta: false },
            json!("curl -fsSL https://ployz.sh | sh"),
            nothing.clone(),
        ),
        (
            FollowUp::UpdateClient { beta: true },
            json!("curl -fsSL https://ployz.sh | sh -s -- beta"),
            nothing.clone(),
        ),
        (
            FollowUp::ShowEnvironment { place: staging() },
            json!({ "tool": "environment_show", "input": scope }),
            call(
                "environment_show",
                json!({ "project": "shop", "environment": "staging" }),
            ),
        ),
        (
            FollowUp::ShowProject { project: project() },
            json!({ "tool": "project_show", "input": { "project": "shop" } }),
            call("environment_show", json!({ "project": "shop" })),
        ),
        (
            FollowUp::UnprotectEnvironment { place: staging() },
            json!({ "tool": "environment_protect", "input": { "project": "shop", "environment": "staging", "protected": false } }),
            fix(DashboardPage::Environments),
        ),
        (
            FollowUp::Remove {
                project: project(),
                environment: Some(environment("staging")),
            },
            json!({ "tool": "remove", "input": { "project": "shop", "environment": "staging" } }),
            nothing.clone(),
        ),
        (
            FollowUp::Remove {
                project: project(),
                environment: None,
            },
            json!({ "tool": "remove", "input": { "project": "shop" } }),
            nothing.clone(),
        ),
        (
            FollowUp::StopKeepingBranch { place: at("fix") },
            json!({ "tool": "keep_branch", "input": { "environment": { "project": "shop", "environment": "fix" }, "kept": false } }),
            nothing,
        ),
    ];
    for (follow_up, next, (call, fix)) in cases {
        assert_eq!(follow_up.cli(), next, "{follow_up:?}");
        let details = follow_up.details();
        assert_eq!(details.get("next"), Some(&next), "{follow_up:?}");
        assert_eq!(FollowUp::of(&details).as_ref(), Some(&follow_up));
        let error = store_error(RpcErrorCode::Conflict, "Refused", details);
        let refusal = from_store(error, &Call::new("deploy", json!({})), Writes::None);
        assert_eq!(refusal.next, call.into_iter().collect::<Vec<_>>(), "{next}");
        assert_eq!(refusal.fix(), fix, "{next}");
        assert_eq!(refusal.details.get("follow_up"), None, "{next}");
        assert_eq!(refusal.details.get("next"), None, "{next}");
    }
}

#[test]
fn a_cli_line_alone_tells_an_agent_nothing() {
    for next in [
        json!("ployz domain add web"),
        json!("ployz server add"),
        json!({ "tool": "environment_show", "input": { "environment": "staging" } }),
    ] {
        let error = store_error(RpcErrorCode::Conflict, "Refused", json!({ "next": next }));
        let refusal = from_store(error, &Call::new("deploy", json!({})), Writes::None);
        assert!(refusal.next.is_empty());
        assert_eq!(refusal.fix(), None);
    }
}

#[test]
fn a_follow_up_keeps_the_fields_it_sits_beside() {
    let details = FollowUp::DomainAdd {
        service: "web".into(),
    }
    .within(json!({ "hostname": "app.example.com" }));
    assert_eq!(
        details,
        json!({
            "hostname": "app.example.com",
            "next": "ployz domain add web",
            "follow_up": { "kind": "domain_add", "service": "web" },
        })
    );
    let mut stripped = details;
    FollowUp::remove_from(&mut stripped);
    assert_eq!(stripped, json!({ "hostname": "app.example.com" }));
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
        FollowUp::ServerAdd.details(),
    );
    let refusal = from_store(error, &call, Writes::Committed);
    assert!(refusal.retry.is_same());
    assert_eq!(refusal.writes, Writes::Committed);
    assert!(refusal.next.is_empty());
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
