//! Each test starts its own postgres:16-alpine and applies the dashboard's real migrations, so the
//! change-log triggers the feed tails are the ones production runs.

use std::collections::HashSet;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use tokio::time::sleep;
use tokio_postgres::{Client, NoTls};
use uuid::Uuid;

use super::*;

struct Postgres {
    container: String,
    url: String,
}

impl Drop for Postgres {
    fn drop(&mut self) {
        drop(
            Command::new("docker")
                .args(["rm", "-f", &self.container])
                .output(),
        );
    }
}

fn docker(args: &[&str]) -> String {
    let out = Command::new("docker")
        .args(args)
        .output()
        .expect("these tests need Docker on PATH");
    assert!(
        out.status.success(),
        "docker {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

/// A fresh database with every `dashboard/drizzle/*/migration.sql` applied in order.
async fn postgres() -> (Postgres, Client) {
    let container = docker(&[
        "run",
        "-d",
        "--rm",
        "--tmpfs",
        "/var/lib/postgresql/data:rw",
        "--publish",
        "127.0.0.1::5432",
        "-e",
        "POSTGRES_PASSWORD=postgres",
        "-e",
        "POSTGRES_DB=ployz_cloud",
        "postgres:16-alpine",
        "-c",
        "fsync=off",
        "-c",
        "synchronous_commit=off",
        "-c",
        "full_page_writes=off",
    ]);
    let port = docker(&["port", &container, "5432/tcp"]);
    let port = port
        .lines()
        .next()
        .unwrap()
        .rsplit(':')
        .next()
        .unwrap()
        .to_owned();
    let pg = Postgres {
        container,
        url: format!("postgres://postgres:postgres@127.0.0.1:{port}/ployz_cloud?sslmode=disable"),
    };
    // initdb's temporary server listens on the socket only, so a TCP answer is the real one.
    let mut client = None;
    for _ in 0..300 {
        if let Ok((c, connection)) = tokio_postgres::connect(&pg.url, NoTls).await {
            tokio::spawn(connection);
            if c.simple_query("SELECT 1").await.is_ok() {
                client = Some(c);
                break;
            }
        }
        sleep(Duration::from_millis(100)).await;
    }
    let client = client.expect("postgres:16-alpine never accepted a connection");
    let drizzle = Path::new(env!("CARGO_MANIFEST_DIR")).join("../drizzle");
    let mut migrations: Vec<_> = std::fs::read_dir(&drizzle)
        .unwrap()
        .map(|entry| entry.unwrap().path().join("migration.sql"))
        .filter(|path| path.exists())
        .collect();
    migrations.sort();
    assert!(migrations.len() >= 6, "found {migrations:?}");
    for migration in &migrations {
        let sql = std::fs::read_to_string(migration).unwrap();
        client
            .batch_execute(&sql)
            .await
            .unwrap_or_else(|e| panic!("{}: {e:?}", migration.display()));
    }
    (pg, client)
}

async fn feed_pg(pg: &Postgres) -> Pg {
    let crypto = Arc::new(rustls::crypto::ring::default_provider());
    Pg::connect(&pg.url, crypto).await.unwrap()
}

async fn org(admin: &Client) -> Uuid {
    let id = Uuid::new_v4();
    admin
        .execute(
            "INSERT INTO organization (id, name, slug) VALUES ($1, 'Org', $2)",
            &[&id, &id.to_string()],
        )
        .await
        .unwrap();
    id
}

async fn check(admin: &Client, org: Uuid, url: &str) -> Uuid {
    admin
        .query_one(
            "INSERT INTO uptime_check (organization_id, name, url) VALUES ($1, 'Check', $2) \
             RETURNING id",
            &[&org, &url],
        )
        .await
        .unwrap()
        .get(0)
}

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
            "INSERT INTO uptime_check (organization_id, name, url, enabled) \
             SELECT $1, 'c' || g, 'https://example.com/' || g, g % 4000 <> 0 \
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

    let (_feed, snapshot) = CheckFeed::load(feed_pg(&pg).await).await.unwrap();
    assert!(snapshot.skipped.is_empty(), "{:?}", snapshot.skipped);
    let got: Vec<Uuid> = snapshot.rows.iter().map(|r| r.check.id.0).collect();
    assert_eq!(got.len(), 20_007, "no row lost or repeated");
    assert_eq!(got.iter().copied().collect::<HashSet<_>>(), want);
}

/// Catches a tail that misses one kind of change: an insert, a revision bump, a disable that
/// leaves the check held, a delete whose id is only in `deleted_ids`, or an incident write.
#[tokio::test]
async fn inserts_edits_disables_deletes_and_incidents_reach_the_map() {
    let (pg, admin) = postgres().await;
    let org = org(&admin).await;
    let edited = check(&admin, org, "https://a.example.com/").await;
    let disabled = check(&admin, org, "https://b.example.com/").await;
    let deleted = check(&admin, org, "https://c.example.com/").await;
    let (mut feed, snapshot) = CheckFeed::load(feed_pg(&pg).await).await.unwrap();
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

    let (mut feed, snapshot) = CheckFeed::load(feed_pg(&pg).await).await.unwrap();
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
    admin
        .batch_execute(
            "CREATE ROLE feed LOGIN PASSWORD 'feed'; \
             GRANT SELECT ON uptime_check, uptime_incident, organization_change TO feed",
        )
        .await
        .unwrap();
    let url = pg.url.replacen("postgres:postgres@", "feed:feed@", 1);
    let crypto = Arc::new(rustls::crypto::ring::default_provider());
    let (mut feed, snapshot) = CheckFeed::load(Pg::connect(&url, crypto).await.unwrap())
        .await
        .unwrap();
    let mut checks = loaded(snapshot);

    admin
        .batch_execute("ALTER ROLE feed CONNECTION LIMIT 0")
        .await
        .unwrap();
    let killed: i64 = admin
        .query_one(
            "SELECT count(pg_terminate_backend(pid)) FROM pg_stat_activity \
             WHERE usename = 'feed'",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(killed, 1, "the feed's one connection");
    let during = check(&admin, org, "https://during.example.com/").await;

    for _ in 0..3 {
        let refused = feed.next().await;
        assert!(matches!(refused, Err(PgError::Transient(_))), "{refused:?}");
    }
    admin
        .batch_execute("ALTER ROLE feed CONNECTION LIMIT -1")
        .await
        .unwrap();
    follow(&mut feed, &mut checks, |c| held(c, during).is_some()).await;
    assert_eq!(checks.len(), 2);
}

/// Catches a feed that ignores `expired` and keeps tailing past changes retention already pruned.
#[tokio::test]
async fn a_cursor_older_than_the_log_reloads_everything() {
    let (pg, admin) = postgres().await;
    let org = org(&admin).await;
    let (mut feed, snapshot) = CheckFeed::load(feed_pg(&pg).await).await.unwrap();
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
    let (mut feed, snapshot) = CheckFeed::load(feed_pg(&pg).await).await.unwrap();
    let mut checks = loaded(snapshot);

    admin
        .execute(
            "INSERT INTO uptime_check (organization_id, name, url) \
             SELECT $1, 'c' || g, 'https://example.com/' || g FROM generate_series(1, 150) g",
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
