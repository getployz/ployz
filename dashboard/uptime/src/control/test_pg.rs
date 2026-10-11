//! The real-Postgres harness every control test shares. Each test starts its own
//! postgres:16-alpine and applies the dashboard's real migrations, so the triggers, constraints
//! and indexes under test are the ones production runs.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::sleep;
use tokio_postgres::{Client, NoTls};
use uuid::Uuid;

use crate::control::pg::Pg;

/// Removes its container on drop, so a failing test leaves nothing running.
pub(crate) struct Postgres {
    container: String,
    pub(crate) url: String,
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

impl Postgres {
    /// A [`Pg`] logged in as `postgres`.
    pub(crate) async fn pg(&self) -> Pg {
        connect(&self.url).await
    }

    /// A login role with `grants` applied, so a test can cut it off alone. Returns its URL.
    pub(crate) async fn role(&self, admin: &Client, name: &str, grants: &str) -> String {
        admin
            .batch_execute(&format!(
                "CREATE ROLE {name} LOGIN PASSWORD '{name}'; {grants}"
            ))
            .await
            .unwrap();
        self.url
            .replacen("postgres:postgres@", &format!("{name}:{name}@"), 1)
    }
}

pub(crate) async fn connect(url: &str) -> Pg {
    let crypto = Arc::new(rustls::crypto::ring::default_provider());
    Pg::connect(url, crypto).await.unwrap()
}

/// Holds an outage open for `role`: a connection limit of 0 refuses reconnects, and its live
/// connections are terminated. Returns how many were.
pub(crate) async fn cut(admin: &Client, role: &str) -> i64 {
    admin
        .batch_execute(&format!("ALTER ROLE {role} CONNECTION LIMIT 0"))
        .await
        .unwrap();
    admin
        .query_one(
            "SELECT count(pg_terminate_backend(pid)) FROM pg_stat_activity WHERE usename = $1",
            &[&role],
        )
        .await
        .unwrap()
        .get(0)
}

pub(crate) async fn restore(admin: &Client, role: &str) {
    admin
        .batch_execute(&format!("ALTER ROLE {role} CONNECTION LIMIT -1"))
        .await
        .unwrap();
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
pub(crate) async fn postgres() -> (Postgres, Client) {
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

pub(crate) async fn org(admin: &Client) -> Uuid {
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

pub(crate) async fn check(admin: &Client, org: Uuid, url: &str) -> Uuid {
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

/// A TCP proxy in front of a test database that, once armed, lets one `COMMIT` reach the server
/// and then closes the connection before the reply comes back: the transaction committed, and the
/// client only sees a dropped connection.
pub(crate) struct LostCommit {
    pub(crate) url: String,
    armed: Arc<AtomicBool>,
    fired: Arc<AtomicBool>,
}

impl LostCommit {
    pub(crate) async fn start(pg: &Postgres) -> LostCommit {
        let upstream = pg
            .url
            .split_once("@")
            .and_then(|(_, rest)| rest.split_once('/'))
            .map(|(host, _)| host.to_owned())
            .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let local = listener.local_addr().unwrap();
        let armed = Arc::new(AtomicBool::new(false));
        let fired = Arc::new(AtomicBool::new(false));
        let (a, f) = (armed.clone(), fired.clone());
        let url = pg.url.replacen(&upstream, &local.to_string(), 1);
        tokio::spawn(async move {
            while let Ok((client, _)) = listener.accept().await {
                let server = TcpStream::connect(&upstream).await.unwrap();
                tokio::spawn(relay(client, server, a.clone(), f.clone()));
            }
        });
        LostCommit { url, armed, fired }
    }

    pub(crate) fn arm(&self) {
        self.armed.store(true, Ordering::SeqCst);
    }

    pub(crate) fn fired(&self) -> bool {
        self.fired.load(Ordering::SeqCst)
    }
}

async fn relay(
    client: TcpStream,
    server: TcpStream,
    armed: Arc<AtomicBool>,
    fired: Arc<AtomicBool>,
) {
    let (mut client_read, mut client_write) = client.into_split();
    let (mut server_read, mut server_write) = server.into_split();
    let replies = tokio::spawn(async move {
        let mut buf = vec![0; 64 * 1024];
        while let Ok(n @ 1..) = server_read.read(&mut buf).await {
            if client_write.write_all(buf.get(..n).unwrap()).await.is_err() {
                break;
            }
        }
    });
    let mut buf = vec![0; 64 * 1024];
    while let Ok(n @ 1..) = client_read.read(&mut buf).await {
        let chunk = buf.get(..n).unwrap();
        let commit = chunk.windows(7).any(|w| w == b"COMMIT\0");
        if commit && armed.swap(false, Ordering::SeqCst) {
            replies.abort();
            server_write.write_all(chunk).await.unwrap();
            // Long enough for the server to commit; the reply goes nowhere.
            sleep(Duration::from_millis(300)).await;
            fired.store(true, Ordering::SeqCst);
            return;
        }
        if server_write.write_all(chunk).await.is_err() {
            break;
        }
    }
    replies.abort();
}
