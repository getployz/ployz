//! Each test runs against its own real Postgres (see `test_pg`), so the change-log triggers the
//! feed tails are the ones production runs.

use std::collections::HashSet;
use std::time::Duration;

use tokio::time::sleep;
use uuid::Uuid;

use super::*;
use crate::control::test_pg::{check, connect, cut, org, postgres, restore};

/// Polls the tail until `done` holds. A change that never arrives fails after about two seconds.
async fn follow(feed: &mut CheckFeed, checks: &mut Checks, done: impl Fn(&Checks) -> bool) {
    for _ in 0..40 {
        checks.apply(feed.next().await.unwrap());
        if done(checks) {
            return;
        }
        sleep(Duration::from_millis(50)).await;
    }
    panic!("the tail never delivered the change; holding {checks:#?}");
}

fn held(checks: &Checks, id: Uuid) -> Option<&CheckRow> {
    checks.get(CheckId(id))
}

fn loaded(snapshot: Snapshot) -> Checks {
    let mut checks = Checks::default();
    checks.apply(FeedEvent::Loaded(snapshot));
    checks
}

/// Catches a keyset off-by-one (a row lost or repeated at the page seam), a load that stops after
/// the first page, and disabled checks being loaded.
#[tokio::test]
async fn the_load_returns_every_enabled_check_across_pages() {
    let (pg, admin) = postgres().await;
    let org = org(&admin).await;
    admin
        .execute(
            "INSERT INTO uptime_check (organization_id, name, url, enabled, environment_id, service_id) \
             SELECT $1, 'c' || g, 'https://example.com/' || g, g % 4000 <> 0, gen_random_uuid()::text, 'svc-' || g \
             FROM generate_series(1, 20012) g",
            &[&org],
        )
        .await
        .unwrap();
    let want: HashSet<Uuid> = admin
        .query("SELECT id FROM uptime_check WHERE enabled", &[])
        .await
        .unwrap()
        .iter()
        .map(|row| row.get(0))
        .collect();
    assert_eq!(want.len(), 20_007, "5 of the 20,012 are disabled");

    let (_feed, snapshot) = CheckFeed::load(pg.pg().await).await.unwrap();
    assert!(snapshot.skipped.is_empty(), "{:?}", snapshot.skipped);
    let got: Vec<Uuid> = snapshot.rows.iter().map(|r| r.check.id.0).collect();
    assert_eq!(got.len(), 20_007, "no row lost or repeated");
    assert_eq!(got.iter().copied().collect::<HashSet<_>>(), want);
}

/// Catches a tail that misses one kind of change: an insert, a revision bump, a disable that
/// leaves the check held, a delete whose id is only in `deleted_ids`, an incident write, or a probe
/// request.
#[tokio::test]
async fn inserts_edits_disables_deletes_and_incidents_reach_the_map() {
    let (pg, admin) = postgres().await;
    let org = org(&admin).await;
    let edited = check(&admin, org, "https://a.example.com/").await;
    let disabled = check(&admin, org, "https://b.example.com/").await;
    let deleted = check(&admin, org, "https://c.example.com/").await;
    let (mut feed, snapshot) = CheckFeed::load(pg.pg().await).await.unwrap();
    let mut checks = loaded(snapshot);
    assert_eq!(checks.len(), 3);

    let inserted = check(&admin, org, "https://d.example.com/").await;
    follow(&mut feed, &mut checks, |c| held(c, inserted).is_some()).await;

    admin
        .execute(
            "UPDATE uptime_check SET url = 'https://a2.example.com/x', \
             revision = revision + 1 WHERE id = $1",
            &[&edited],
        )
        .await
        .unwrap();
    follow(&mut feed, &mut checks, |c| {
        held(c, edited).is_some_and(|r| r.check.revision == Revision(2))
    })
    .await;
    let row = held(&checks, edited).unwrap();
    assert_eq!(&*row.check.url, "https://a2.example.com/x");
    assert_eq!(row.check.target.path_and_query, "/x");

    admin
        .execute(
            "UPDATE uptime_check SET enabled = false, revision = revision + 1 WHERE id = $1",
            &[&disabled],
        )
        .await
        .unwrap();
    follow(&mut feed, &mut checks, |c| held(c, disabled).is_none()).await;

    admin
        .execute("DELETE FROM uptime_check WHERE id = $1", &[&deleted])
        .await
        .unwrap();
    follow(&mut feed, &mut checks, |c| held(c, deleted).is_none()).await;

    let incident = Uuid::new_v4();
    admin
        .execute(
            "INSERT INTO uptime_incident (id, organization_id, check_id, check_revision, \
             opened_at, opened_by) VALUES ($1, $2, $3, 2, now(), '{}')",
            &[&incident, &org, &edited],
        )
        .await
        .unwrap();
    follow(&mut feed, &mut checks, |c| {
        held(c, edited).is_some_and(|r| r.open_incident.is_some())
    })
    .await;
    let open = held(&checks, edited)
        .unwrap()
        .open_incident
        .clone()
        .unwrap();
    assert_eq!(open.id, IncidentId(incident));
    assert_eq!(open.revision, Revision(2));

    admin
        .execute(
            "UPDATE uptime_check SET probe_requested_at = now() WHERE id = $1",
            &[&edited],
        )
        .await
        .unwrap();
    follow(&mut feed, &mut checks, |c| {
        held(c, edited).is_some_and(|r| r.probe_requested.is_some())
    })
    .await;
    let row = held(&checks, edited).unwrap();
    assert_eq!(row.check.revision, Revision(2));
    assert!(row.open_incident.is_some());

    let ids: HashSet<Uuid> = [edited, inserted].into();
    assert_eq!(checks.len(), 2);
    assert!(ids.iter().all(|id| held(&checks, *id).is_some()));
}

/// Catches a feed that dies on, or holds, a row the checker can't probe, and a feed that keeps
/// probing a stale target after an edit makes the row malformed.
#[tokio::test]
async fn a_malformed_row_is_skipped_while_the_others_load() {
    let (pg, admin) = postgres().await;
    let org = org(&admin).await;
    let good = check(&admin, org, "https://a.example.com/").await;
    // The database allows any port; the checker probes only 80 and 443.
    let bad = check(&admin, org, "https://b.example.com:8443/").await;
    let other = check(&admin, org, "http://c.example.com/").await;

    let (mut feed, snapshot) = CheckFeed::load(pg.pg().await).await.unwrap();
    assert_eq!(snapshot.skipped.len(), 1, "{:?}", snapshot.skipped);
    let skipped = snapshot.skipped.first().unwrap();
    assert_eq!(skipped.check, CheckId(bad));
    assert!(matches!(skipped.reason, BadRow::Target(_)), "{skipped:?}");
    let mut checks = loaded(snapshot);
    assert_eq!(checks.len(), 2);
    assert!(held(&checks, good).is_some() && held(&checks, other).is_some());

    admin
        .execute(
            "UPDATE uptime_check SET url = 'https://a.example.com:8080/', \
             revision = revision + 1 WHERE id = $1",
            &[&good],
        )
        .await
        .unwrap();
    follow(&mut feed, &mut checks, |c| held(c, good).is_none()).await;
    assert!(held(&checks, other).is_some());

    admin
        .execute(
            "UPDATE uptime_check SET url = 'https://b.example.com/', \
             revision = revision + 1 WHERE id = $1",
            &[&bad],
        )
        .await
        .unwrap();
    follow(&mut feed, &mut checks, |c| held(c, bad).is_some()).await;
}

/// Catches a cursor that advances on a failed window, or restarts at the horizon after a
/// reconnect, either of which drops a change committed while the connection was down.
#[tokio::test]
async fn a_dropped_connection_resumes_without_losing_a_change() {
    let (pg, admin) = postgres().await;
    let org = org(&admin).await;
    check(&admin, org, "https://a.example.com/").await;
    // Its own role, so the outage can be held open: a connection limit of 0 refuses reconnects.
    let url = pg
        .role(
            &admin,
            "feed",
            "GRANT SELECT ON uptime_check, uptime_incident, organization_change TO feed",
        )
        .await;
    let (mut feed, snapshot) = CheckFeed::load(connect(&url).await).await.unwrap();
    let mut checks = loaded(snapshot);

    assert_eq!(cut(&admin, "feed").await, 1, "the feed's one connection");
    let during = check(&admin, org, "https://during.example.com/").await;

    for _ in 0..3 {
        let refused = feed.next().await;
        assert!(matches!(refused, Err(PgError::Transient(_))), "{refused:?}");
    }
    restore(&admin, "feed").await;
    follow(&mut feed, &mut checks, |c| held(c, during).is_some()).await;
    assert_eq!(checks.len(), 2);
}

/// Catches a feed that ignores `expired` and keeps tailing past changes retention already pruned.
#[tokio::test]
async fn a_cursor_older_than_the_log_reloads_everything() {
    let (pg, admin) = postgres().await;
    let org = org(&admin).await;
    let (mut feed, snapshot) = CheckFeed::load(pg.pg().await).await.unwrap();
    assert!(snapshot.rows.is_empty());
    let pruned = check(&admin, org, "https://a.example.com/").await;
    let kept = check(&admin, org, "https://b.example.com/").await;
    // Retention prunes oldest first: everything before `kept`'s change, `pruned`'s included.
    let removed = admin
        .execute(
            "DELETE FROM organization_change WHERE xid < \
             (SELECT xid FROM organization_change WHERE $1 = ANY(changed_ids))",
            &[&kept.to_string()],
        )
        .await
        .unwrap();
    assert!(removed >= 1);

    let event = feed.next().await.unwrap();
    let FeedEvent::Loaded(snapshot) = event else {
        panic!("expected a full reload, got {event:?}");
    };
    let ids: HashSet<Uuid> = snapshot.rows.iter().map(|r| r.check.id.0).collect();
    assert_eq!(ids, [pruned, kept].into());
}

/// Catches a feed that ignores `full_orgs`: a statement over 100 rows logs no ids, so only the
/// org-wide re-read sees its inserts and its disables, and other orgs must be left alone.
#[tokio::test]
async fn a_bulk_statement_replaces_its_org_and_no_other() {
    let (pg, admin) = postgres().await;
    let bulk = org(&admin).await;
    let quiet = org(&admin).await;
    let untouched = check(&admin, quiet, "https://quiet.example.com/").await;
    let (mut feed, snapshot) = CheckFeed::load(pg.pg().await).await.unwrap();
    let mut checks = loaded(snapshot);

    admin
        .execute(
            "INSERT INTO uptime_check (organization_id, name, url, environment_id, service_id) \
             SELECT $1, 'c' || g, 'https://example.com/' || g, gen_random_uuid()::text, 'svc-' || g \
             FROM generate_series(1, 150) g",
            &[&bulk],
        )
        .await
        .unwrap();
    follow(&mut feed, &mut checks, |c| c.len() == 151).await;

    admin
        .execute(
            "UPDATE uptime_check SET enabled = false, revision = revision + 1 \
             WHERE organization_id = $1",
            &[&bulk],
        )
        .await
        .unwrap();
    follow(&mut feed, &mut checks, |c| c.len() == 1).await;
    assert!(held(&checks, untouched).is_some());
}
