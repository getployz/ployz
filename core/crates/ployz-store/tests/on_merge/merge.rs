use ployz_core::{RpcErrorCode, ServiceName};
use ployz_store::{
    Actor, CheckSuite, ConfigStore, EnvironmentName, EnvironmentRef, EnvironmentsQuery,
    HistoryAction, HistoryPreviewQuery, NeverSync, OnMergeCheck, OnMergeConclusion, ProjectName,
    PullRequest, Ride, ServiceLineageId, SetPrPlan, StageHistory, SyncChanges, SyncQuery,
    SystemEvent,
};
use serde_json::{Value, json};

use crate::backend;
use crate::support::{
    HEAD, LATER, MERGE, at, check, deployed, exclude, history, labels, merge_value, merged,
    merged_within, on_merge, preview_listed, production_diff, pull, push, row, rows, save, set,
    shop, shop_with, system, unset, uuid, value, watching,
};

#[test]
fn every_change_rides_over_what_production_changed_since_a_removal_too() {
    let (store, who) = shop();
    assert_eq!(labels(&store, &who), []);

    // Unsaved changes ride: a change, a new variable and a removal.
    set(
        &store,
        &who,
        "pr-5",
        &[("web.env.A", json!("2")), ("web.env.B", json!("new"))],
    );
    unset(&store, &who, "pr-5", "web.env.C");
    // production changes A after pr-5 started: the merge lands over it, flagged.
    set(&store, &who, "production", &[("web.env.A", json!("3"))]);
    save(&store, &who, "production");

    let a = row(&store, &who, "web.env.A");
    assert_eq!(
        (a.production, a.lands, a.preview, a.ride, a.conflict),
        (json!("3"), json!("2"), json!("2"), Ride::Rides, true)
    );
    let b = row(&store, &who, "web.env.B");
    assert_eq!(
        (b.production, b.lands, b.ride, b.conflict),
        (Value::Null, json!("new"), Ride::Rides, false)
    );
    let c = row(&store, &who, "web.env.C");
    assert_eq!(
        (c.production, c.lands, c.preview, c.ride),
        (json!("1"), Value::Null, Value::Null, Ride::Rides)
    );
    assert_eq!(rows(&store, &who).len(), 3);
}

#[test]
fn an_excluded_row_stays_listed_but_never_rides_until_included_again() {
    let (store, who) = shop();
    set(&store, &who, "pr-5", &[("web.env.A", json!("2"))]);
    unset(&store, &who, "pr-5", "web.env.C");

    // A removed row is excluded by name too: it is in what pr-5 started from.
    exclude(&store, &who, "web.env.A", false);
    exclude(&store, &who, "web.env.C", false);
    assert_eq!(
        labels(&store, &who),
        [
            ("web.env.A".to_owned(), Ride::Excluded),
            ("web.env.C".to_owned(), Ride::Excluded)
        ]
    );
    // An Excluded row takes no Edit.
    let refused = store
        .write(&who, &merge_value("web.env.A", Some("9")))
        .unwrap_err();
    assert_eq!(refused.code, RpcErrorCode::InvalidArgument, "{refused:?}");

    exclude(&store, &who, "web.env.C", true);
    assert_eq!(row(&store, &who, "web.env.C").ride, Ride::Rides);
}

#[test]
fn productions_own_never_sync_mark_keeps_its_value() {
    let (store, who) = shop();
    store
        .write(
            &who,
            &NeverSync {
                environment: at("production"),
                rows: vec!["web.env.D".into()],
                off: false,
            },
        )
        .unwrap();
    set(&store, &who, "pr-5", &[("web.env.D", json!("2"))]);
    let d = row(&store, &who, "web.env.D");
    assert_eq!((d.ride, d.production), (Ride::ProductionKeeps, json!("1")));
}

#[test]
fn a_new_secret_lands_not_set_unless_edited_and_its_edit_stays_sealed() {
    let (store, who) = shop();
    set(
        &store,
        &who,
        "pr-5",
        &[("web.env.TOKEN", json!({ "secret": "test-token" }))],
    );
    let token = row(&store, &who, "web.env.TOKEN");
    assert_eq!(
        (token.preview, token.lands, token.ride),
        (
            json!({ "secret": true }),
            json!({ "secret": false }),
            Ride::Rides
        )
    );

    let edited = store
        .write(&who, &merge_value("web.env.TOKEN", Some("prod-token")))
        .unwrap();
    let token = &edited.destinations[0].rows[0];
    assert_eq!(
        (&token.lands, token.ride),
        (&json!({ "secret": true }), Ride::Edited)
    );
    assert!(!format!("{edited:?}").contains("prod-token"));

    // Taking the Edit back lands it Not set again.
    store
        .write(&who, &merge_value("web.env.TOKEN", None))
        .unwrap();
    let token = row(&store, &who, "web.env.TOKEN");
    assert_eq!(
        (token.lands, token.ride),
        (json!({ "secret": false }), Ride::Rides)
    );
}

#[test]
fn an_edit_gives_a_riding_variable_its_value_and_only_a_variable() {
    let (store, who) = shop();
    set(
        &store,
        &who,
        "pr-5",
        &[
            ("web.env.A", json!("2")),
            ("web.startCommand", json!("serve")),
        ],
    );
    store
        .write(&who, &merge_value("web.env.A", Some("prod")))
        .unwrap();
    let a = row(&store, &who, "web.env.A");
    assert_eq!(
        (a.lands, a.preview, a.ride),
        (json!("prod"), json!("2"), Ride::Edited)
    );

    for label in ["web", "web.startCommand", "web.env.NOPE"] {
        let refused = store.write(&who, &merge_value(label, Some("x")));
        assert!(refused.is_err(), "{label}: {refused:?}");
    }

    // pr-5 puts A back: the row leaves the list and its Edit is ignored.
    set(&store, &who, "pr-5", &[("web.env.A", json!("1"))]);
    assert_eq!(
        labels(&store, &who),
        [("web.startCommand".to_owned(), Ride::Rides)]
    );
}

#[test]
fn sizing_never_rides() {
    let (store, who) = shop();
    set(&store, &who, "pr-5", &[("web.replicas", json!(3))]);
    assert_eq!(labels(&store, &who), []);
}

#[test]
fn a_row_synced_now_leaves_the_list() {
    let (store, who) = shop();
    set(
        &store,
        &who,
        "pr-5",
        &[("web.env.A", json!("2")), ("web.env.B", json!("new"))],
    );
    let review = store
        .read(
            &who,
            &SyncQuery {
                from: at("pr-5"),
                into: Some(at("production")),
            },
        )
        .unwrap();
    store
        .write(
            &who,
            &SyncChanges {
                from: at("pr-5"),
                into: Some(at("production")),
                close_after: false,
                version: review.version,
                picks: Some(vec!["web.env.A".into()]),
                skip: Vec::new(),
                values: std::collections::BTreeMap::new(),
                id: None,
            },
        )
        .unwrap();
    save(&store, &who, "production");
    assert_eq!(
        labels(&store, &who),
        [("web.env.B".to_owned(), Ride::Rides)]
    );
}

#[test]
fn retargeting_the_pull_request_forgets_its_edits() {
    let (store, who) = shop();
    set(&store, &who, "pr-5", &[("web.env.A", json!("2"))]);
    store
        .write(&who, &merge_value("web.env.A", Some("prod")))
        .unwrap();
    assert_eq!(row(&store, &who, "web.env.A").ride, Ride::Edited);

    // Onto `release`, which nothing deploys, the merge lands nowhere.
    pull(&store, &who, "release", "2026-09-29T10:00:01Z");
    assert_eq!(on_merge(&store, &who).destinations, []);
    pull(&store, &who, "main", "2026-09-29T10:00:02Z");
    assert_eq!(row(&store, &who, "web.env.A").ride, Ride::Rides);
}

/// Whether the listing reads `pr-5` merged; none when it isn't listed.
fn listed_merged(store: &ConfigStore, who: &Actor) -> Option<bool> {
    store
        .read(who, &EnvironmentsQuery::default())
        .unwrap()
        .environments
        .iter()
        .find(|listing| listing.name.as_str() == "pr-5")
        .map(|listing| listing.merged)
}

#[test]
fn the_merge_lands_every_riding_row_in_one_saved_revision_that_undo_takes_back() {
    let (store, who) = shop();
    set(
        &store,
        &who,
        "pr-5",
        &[("web.env.A", json!("2")), ("web.env.B", json!("new"))],
    );
    unset(&store, &who, "pr-5", "web.env.D");
    // production changed A since: the merge lands over it.
    set(&store, &who, "production", &[("web.env.A", json!("5"))]);
    save(&store, &who, "production");
    let before = history(&store, &who).len();

    let automated = merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    let revisions = history(&store, &who);
    assert_eq!(revisions.len(), before + 1);
    let landed = revisions
        .iter()
        .find(|revision| revision.message.as_deref() == Some("Merged #5"))
        .unwrap();
    // Saved and Working agree: nothing is left to save.
    assert!(production_diff(&store, &who).published);
    assert_eq!(
        [
            value(&store, &who, "production", "web.env.A"),
            value(&store, &who, "production", "web.env.B"),
            value(&store, &who, "production", "web.env.C"),
            value(&store, &who, "production", "web.env.D"),
        ],
        [Some(json!("2")), Some(json!("new")), Some(json!("1")), None]
    );
    // It closes as before, once it landed.
    assert!(
        automated.closing.len() + automated.removed.len() == 1 || !preview_listed(&store, &who),
        "{automated:?}"
    );

    // Undo, from History, puts production back.
    let query = HistoryPreviewQuery {
        environment: at("production"),
        revision: landed.revision,
        action: HistoryAction::Undo,
        change: None,
    };
    let preview = store.read(&who, &query).unwrap();
    store
        .write(
            &who,
            &StageHistory {
                environment: at("production"),
                revision: query.revision,
                action: query.action,
                change: None,
                version: preview.version,
                accept_overwrite: false,
            },
        )
        .unwrap();
    assert_eq!(
        [
            value(&store, &who, "production", "web.env.A"),
            value(&store, &who, "production", "web.env.B"),
            value(&store, &who, "production", "web.env.D"),
        ],
        [Some(json!("5")), None, Some(json!("1"))]
    );
}

#[test]
fn the_merge_lands_edits_and_not_set_secrets_and_leaves_what_stays() {
    let (store, who) = shop();
    store
        .write(
            &who,
            &NeverSync {
                environment: at("production"),
                rows: vec!["web.env.D".into()],
                off: false,
            },
        )
        .unwrap();
    set(
        &store,
        &who,
        "pr-5",
        &[
            ("web.env.A", json!("2")),
            ("web.env.B", json!("new")),
            ("web.env.C", json!("2")),
            ("web.env.D", json!("2")),
            ("web.env.TOKEN", json!({ "secret": "test-token" })),
        ],
    );
    exclude(&store, &who, "web.env.B", false);
    store
        .write(&who, &merge_value("web.env.C", Some("prod")))
        .unwrap();

    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    assert_eq!(
        [
            value(&store, &who, "production", "web.env.A"),
            value(&store, &who, "production", "web.env.B"),
            value(&store, &who, "production", "web.env.C"),
            value(&store, &who, "production", "web.env.D"),
            value(&store, &who, "production", "web.env.TOKEN"),
        ],
        [
            Some(json!("2")),
            None,
            Some(json!("prod")),
            Some(json!("1")),
            Some(json!({ "secret": false })),
        ]
    );
}

#[test]
fn an_edit_of_a_plain_variable_the_preview_made_secret_never_lands_in_plain_text() {
    let (store, who) = shop();
    set(&store, &who, "pr-5", &[("web.env.NEW", json!("2"))]);
    store
        .write(&who, &merge_value("web.env.NEW", Some("prod-token")))
        .unwrap();
    set(
        &store,
        &who,
        "pr-5",
        &[("web.env.NEW", json!({ "secret": "test-token" }))],
    );

    // The Edit was for a plain value: the secret lands Not set, as unedited.
    let new = row(&store, &who, "web.env.NEW");
    assert_eq!(
        (new.lands, new.ride),
        (json!({ "secret": false }), Ride::Rides)
    );
    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    assert_eq!(
        value(&store, &who, "production", "web.env.NEW"),
        Some(json!({ "secret": false }))
    );
}

#[test]
fn an_edit_of_a_secret_the_preview_made_plain_lands_the_preview_value() {
    let (store, who) = shop();
    set(
        &store,
        &who,
        "pr-5",
        &[("web.env.NEW", json!({ "secret": "test-token" }))],
    );
    store
        .write(&who, &merge_value("web.env.NEW", Some("prod-token")))
        .unwrap();
    // A secret never turns plain in place: the preview unsets it, then sets it.
    unset(&store, &who, "pr-5", "web.env.NEW");
    set(&store, &who, "pr-5", &[("web.env.NEW", json!("2"))]);

    let new = row(&store, &who, "web.env.NEW");
    assert_eq!((new.lands, new.ride), (json!("2"), Ride::Rides));
    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    assert_eq!(
        value(&store, &who, "production", "web.env.NEW"),
        Some(json!("2"))
    );
}

#[test]
fn the_merge_lands_once_whatever_order_its_events_come_in() {
    let (store, who) = shop();
    set(&store, &who, "pr-5", &[("web.env.A", json!("2"))]);
    // GitHub's merged event came in after a later read: still the first merge.
    pull(&store, &who, "main", "2026-09-29T12:00:00Z");
    let stale = merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    assert_eq!(stale.checks.len(), 1, "{stale:?}");
    let landed = history(&store, &who).len();
    assert_eq!(
        value(&store, &who, "production", "web.env.A"),
        Some(json!("2"))
    );

    // production moves on; a replay or a later read lands nothing again.
    set(&store, &who, "production", &[("web.env.A", json!("7"))]);
    save(&store, &who, "production");
    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    merged(&store, &who, "2026-09-29T13:00:00Z", MERGE);
    assert_eq!(history(&store, &who).len(), landed + 1);
    assert_eq!(
        value(&store, &who, "production", "web.env.A"),
        Some(json!("7"))
    );
}

#[test]
fn productions_staged_edit_stays_staged_over_what_landed() {
    let (store, who) = shop();
    set(
        &store,
        &who,
        "pr-5",
        &[("web.env.A", json!("2")), ("web.env.B", json!("new"))],
    );
    set(&store, &who, "production", &[("web.env.A", json!("9"))]);

    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    assert_eq!(
        value(&store, &who, "production", "web.env.A"),
        Some(json!("9"))
    );
    assert_eq!(
        value(&store, &who, "production", "web.env.B"),
        Some(json!("new"))
    );
    // Saved holds the landed A; the staged 9 wins at the next Save.
    let staged = serde_json::to_value(production_diff(&store, &who).draft_changes).unwrap();
    assert_eq!(
        staged[0]["settings"],
        json!([{
            "path": "web.env.A", "kind": "update", "before": "2", "after": "9",
            "canRestore": true, "row": staged[0]["settings"][0]["row"],
        }]),
        "{staged}"
    );
}

#[test]
fn what_landed_deploys_with_the_next_push_if_the_merge_came_first() {
    let (store, who) = shop();
    set(
        &store,
        &who,
        "production",
        &[("web.watchPaths", json!(["web/**"]))],
    );
    system(&store, &who, &push(None, HEAD, None));
    set(&store, &who, "pr-5", &[("web.env.A", json!("2"))]);

    let landed = merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    assert!(landed.admitted.is_empty(), "{landed:?}");
    // The merge's push changed nothing web watches: web deploys for what landed.
    let pushed = system(&store, &who, &push(Some(HEAD), MERGE, Some(&["README.md"])));
    assert_eq!(deployed(&pushed), [["web"]]);
    // Once.
    let next = system(
        &store,
        &who,
        &push(Some(MERGE), LATER, Some(&["README.md"])),
    );
    assert!(next.admitted.is_empty(), "{next:?}");
}

#[test]
fn what_landed_deploys_at_once_if_the_push_came_first() {
    let (store, who) = shop();
    set(
        &store,
        &who,
        "production",
        &[("web.watchPaths", json!(["web/**"]))],
    );
    system(&store, &who, &push(None, HEAD, None));
    set(&store, &who, "pr-5", &[("web.env.A", json!("2"))]);
    let pushed = system(&store, &who, &push(Some(HEAD), MERGE, Some(&["README.md"])));
    assert!(pushed.admitted.is_empty(), "{pushed:?}");

    let landed = merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    assert_eq!(deployed(&landed), [["web"]]);
}

#[test]
fn a_merge_whose_commit_was_never_pushed_deploys_from_the_head_that_contains_it() {
    let (store, who) = shop();
    watching(&store, &who);
    set(&store, &who, "pr-5", &[("web.env.A", json!("2"))]);
    // Two merges in quick succession: Cloud read the branch at the second, so the
    // Store never heard of the first.
    let later = system(&store, &who, &push(Some(HEAD), LATER, Some(&["README.md"])));
    assert!(later.admitted.is_empty(), "{later:?}");

    let landed = merged_within(&store, &who, "2026-09-29T11:00:00Z", MERGE, Some(LATER));
    assert_eq!(deployed(&landed), [["web"]]);
}

#[test]
fn a_merge_contained_in_a_head_the_store_has_not_seen_waits_for_its_push() {
    let (store, who) = shop();
    watching(&store, &who);
    set(&store, &who, "pr-5", &[("web.env.A", json!("2"))]);

    let landed = merged_within(&store, &who, "2026-09-29T11:00:00Z", MERGE, Some(LATER));
    assert!(landed.admitted.is_empty(), "{landed:?}");
    let pushed = system(&store, &who, &push(Some(HEAD), LATER, Some(&["README.md"])));
    assert_eq!(deployed(&pushed), [["web"]]);
}

#[test]
fn what_landed_deploys_with_auto_deploy_off() {
    let (store, who) = shop();
    set(
        &store,
        &who,
        "production",
        &[("web.autoDeploy", json!("false"))],
    );
    system(&store, &who, &push(None, HEAD, None));
    set(&store, &who, "pr-5", &[("web.env.A", json!("2"))]);
    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    // web follows no push, but its config changed: the merge's push deploys it.
    let pushed = system(&store, &who, &push(Some(HEAD), MERGE, Some(&["README.md"])));
    assert_eq!(deployed(&pushed), [["web"]]);
    assert_eq!(
        value(&store, &who, "production", "web.env.A"),
        Some(json!("2"))
    );
}

#[test]
fn what_landed_waits_for_ci_with_the_push() {
    let (store, who) = shop();
    set(
        &store,
        &who,
        "production",
        &[
            ("web.watchPaths", json!(["web/**"])),
            ("web.waitForCi", json!(true)),
        ],
    );
    system(&store, &who, &push(None, HEAD, None));
    set(&store, &who, "pr-5", &[("web.env.A", json!("2"))]);
    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    let pushed = system(&store, &who, &push(Some(HEAD), MERGE, Some(&["README.md"])));
    assert_eq!(pushed.waiting.len(), 1, "{pushed:?}");
    let passed = system(
        &store,
        &who,
        &SystemEvent::CheckSuite(CheckSuite {
            repository_id: backend::repo_id(11),
            suite: 1,
            head: backend::sha(MERGE),
            status: serde_json::from_value(json!("completed")).unwrap(),
            conclusion: Some(serde_json::from_value(json!("success")).unwrap()),
            updated: ployz_store::GithubTimestamp::parse("2026-09-29T11:30:00Z").unwrap(),
        }),
    );
    assert_eq!(deployed(&passed), [["web"]]);
}

#[test]
fn a_merge_into_a_branch_nothing_deploys_keeps_the_preview() {
    let (store, who) = shop();
    set(&store, &who, "pr-5", &[("web.env.A", json!("2"))]);
    pull(&store, &who, "release", "2026-09-29T10:00:01Z");
    let automated = system(
        &store,
        &who,
        &SystemEvent::PullRequest(PullRequest {
            repository_id: backend::repo_id(11),
            number: backend::pr_number(5),
            title: "Add search".into(),
            author: "ada".into(),
            author_id: Some(backend::ada()),
            bot: false,
            head_branch: backend::git_branch("search"),
            head_repository_id: Some(backend::repo_id(11)),
            head: backend::sha(HEAD),
            target_branch: backend::git_branch("release"),
            commits: 1,
            open: false,
            merge_commit: Some(backend::sha(MERGE)),
            target_head: None,
            updated: ployz_store::GithubTimestamp::parse("2026-09-29T11:00:00Z").unwrap(),
        }),
    );
    // Nothing landed its change anywhere, so the preview holding it stays.
    assert!(
        automated.closing.is_empty() && automated.removed.is_empty(),
        "{automated:?}"
    );
    assert_eq!(listed_merged(&store, &who), Some(true));
    assert_eq!(value(&store, &who, "pr-5", "web.env.A"), Some(json!("2")));
}

#[test]
fn a_merge_production_cant_take_lands_nothing_and_keeps_the_preview() {
    let (store, who) = shop();
    // A rides naming api, which stays here.
    store
        .write(
            &who,
            &ployz_store::CreateService {
                id: ServiceLineageId::parse(uuid(9)).unwrap(),
                environment: at("pr-5"),
                name: ServiceName::parse("api").unwrap(),
                image: Some("api:1".into()),
                template: None,
            },
        )
        .unwrap();
    set(
        &store,
        &who,
        "pr-5",
        &[("web.env.A", json!("${{ api.PLOYZ_PRIVATE_DOMAIN }}"))],
    );
    exclude(&store, &who, "api", false);
    let before = history(&store, &who).len();
    assert_eq!(listed_merged(&store, &who), Some(false));

    let automated = merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    assert_eq!(automated.skipped.len(), 1, "{automated:?}");
    assert!(
        automated.skipped[0]
            .reason
            .starts_with("Didn't land #5 in production: "),
        "{automated:?}"
    );
    assert_eq!(history(&store, &who).len(), before);
    assert_eq!(
        value(&store, &who, "production", "web.env.A"),
        Some(json!("1"))
    );
    assert!(automated.closing.is_empty() && automated.removed.is_empty());
    // Listed while it exists, read merged.
    assert_eq!(listed_merged(&store, &who), Some(true));
    let stuck = check(&store, &who).unwrap();
    assert_eq!(
        (stuck.conclusion, stuck.title.as_str()),
        (OnMergeConclusion::Neutral, "Didn't land in production")
    );
    assert!(
        stuck
            .summary
            .contains("web references a removed Service (A)"),
        "{stuck:?}"
    );
    // It links production's History: the PR Environment may be gone.
    assert_eq!(
        stuck.merged_into,
        Some(EnvironmentRef {
            project: Some(ProjectName::parse("shop").unwrap()),
            environment: Some(EnvironmentName::parse("production").unwrap()),
        })
    );
}

#[test]
fn a_kept_preview_has_nothing_left_on_merge_once_it_landed() {
    let (store, who) = shop();
    store
        .write(
            &who,
            &SetPrPlan {
                project: None,
                repository: backend::repo_name("acme/web"),
                enabled: None,
                start_from: None,
                copy: None,
                setup: None,
                remove_on_close: Some(false),
                include_bots: None,
            },
        )
        .unwrap();
    set(&store, &who, "pr-5", &[("web.env.A", json!("2"))]);
    let main = backend::git_branch("main");
    assert_eq!(
        store
            .awaiting_merge(&who.organization, backend::repo_id(11), &main)
            .unwrap(),
        [backend::pr_number(5)]
    );

    merged(&store, &who, "2026-09-29T11:00:00Z", MERGE);
    assert!(preview_listed(&store, &who));
    assert!(
        store
            .awaiting_merge(&who.organization, backend::repo_id(11), &main)
            .unwrap()
            .is_empty()
    );
    let on_merge = on_merge(&store, &who);
    assert!(
        on_merge
            .destinations
            .iter()
            .all(|destination| destination.rows.is_empty()),
        "{on_merge:?}"
    );
    assert_eq!(check(&store, &who), None);
}

#[test]
fn the_check_says_what_lands_and_reads_neutral_over_a_conflict_or_a_missing_secret() {
    let (store, who) = shop();
    assert_eq!(
        check(&store, &who),
        Some(OnMergeCheck {
            conclusion: OnMergeConclusion::Success,
            title: "No config changes".into(),
            summary: "No config changes.".into(),
            merged_into: None,
        })
    );

    set(
        &store,
        &who,
        "pr-5",
        &[("web.env.A", json!("2")), ("web.env.B", json!("new"))],
    );
    unset(&store, &who, "pr-5", "web.env.C");
    assert_eq!(
        check(&store, &who),
        Some(OnMergeCheck {
            conclusion: OnMergeConclusion::Success,
            title: "3 config changes".into(),
            summary: "- `web · A`: 1 → 2\n- `web · B`: new → new\n- `web · C`: 1 → removed".into(),
            merged_into: None,
        })
    );

    // production changed A since: the merge lands over it, said apart.
    set(&store, &who, "production", &[("web.env.A", json!("3"))]);
    save(&store, &who, "production");
    let conflict = check(&store, &who).unwrap();
    assert_eq!(conflict.conclusion, OnMergeConclusion::Neutral);
    assert!(
        conflict.summary.contains(
            "### Changed in production\n\n- `web · A`: production 3 → 2\n\n\
             The merge sets these. Undo in Ployz History reverts it."
        ),
        "{conflict:?}"
    );
}

#[test]
fn the_check_names_a_missing_secret_without_its_value_and_what_stays_here() {
    let (store, who) = shop();
    set(
        &store,
        &who,
        "pr-5",
        &[
            ("web.env.TOKEN", json!({ "secret": "test-token" })),
            ("web.env.D", json!("2")),
        ],
    );
    exclude(&store, &who, "web.env.D", false);
    let missing = check(&store, &who).unwrap();
    assert_eq!(
        missing,
        OnMergeCheck {
            conclusion: OnMergeConclusion::Neutral,
            title: "1 config change · TOKEN not set".into(),
            summary: "- `web · TOKEN`: not set\n\n### Stays in pr-5\n\n- `web · D`: 1 → 2".into(),
            merged_into: None,
        }
    );

    // An Edit gives it a value, which the check never shows.
    store
        .write(&who, &merge_value("web.env.TOKEN", Some("prod-token")))
        .unwrap();
    let edited = check(&store, &who).unwrap();
    assert_eq!(
        (edited.conclusion, edited.title.as_str()),
        (OnMergeConclusion::Success, "1 config change")
    );
    assert!(edited.summary.contains("`web · TOKEN`: set"), "{edited:?}");
    assert!(!format!("{edited:?}").contains("prod-token"));
}

#[test]
fn the_check_reads_a_secret_the_preview_removed_as_removed() {
    let (store, who) = shop_with(|store, who| {
        set(
            store,
            who,
            "production",
            &[("web.env.TOKEN", json!({ "secret": "prod-token" }))],
        );
        save(store, who, "production");
    });
    unset(&store, &who, "pr-5", "web.env.TOKEN");
    let removed = check(&store, &who).unwrap();
    assert_eq!(
        removed.summary, "- `web · TOKEN`: set → removed",
        "{removed:?}"
    );
}

#[test]
fn the_check_says_nothing_deploys_from_a_branch_no_environment_tracks() {
    let (store, who) = shop();
    set(&store, &who, "pr-5", &[("web.env.A", json!("2"))]);
    pull(&store, &who, "release", "2026-09-29T10:00:01Z");
    assert_eq!(
        check(&store, &who),
        Some(OnMergeCheck {
            conclusion: OnMergeConclusion::Neutral,
            title: "Nothing deploys from release".into(),
            summary: "No Environment deploys from `release`, so the merge lands nowhere. \
                      Changes stay in the preview."
                .into(),
            merged_into: None,
        })
    );
}
