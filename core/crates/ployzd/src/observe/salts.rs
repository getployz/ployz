use std::{
    collections::HashMap,
    fs, io,
    path::{Path, PathBuf},
    sync::{PoisonError, RwLock},
    time::Duration,
};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ployz_core::Namespace;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};

use super::layout::{StoreRoot, create_store_dir};

pub(super) const FRESH_FOR: Duration = Duration::from_secs(60 * 60);
const SALT_BYTES: usize = 32;

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub(super) struct Salt {
    #[serde(deserialize_with = "day")]
    pub(super) day: String,
    pub(super) start: i64,
    pub(super) end: i64,
    #[serde(with = "salt_bytes")]
    pub(super) salt: [u8; SALT_BYTES],
}

#[derive(Debug, Deserialize, Serialize)]
struct Site {
    namespace: Namespace,
    salts: Vec<Salt>,
}

#[derive(Debug, Deserialize)]
struct Ack {
    sites: Vec<serde_json::Value>,
}

#[derive(Debug, Deserialize, Serialize)]
struct Persisted {
    fetched: i64,
    sites: Vec<Site>,
}

#[derive(Debug, PartialEq)]
pub(super) enum Lookup {
    Off,
    Gap,
    Covered { day: String, salt: [u8; SALT_BYTES] },
}

#[derive(Debug, Default)]
struct Cache {
    fetched: Option<i64>,
    sites: HashMap<Namespace, Vec<Salt>>,
}

#[derive(Debug)]
pub(super) struct Salts {
    path: PathBuf,
    cache: RwLock<Cache>,
}

impl Salts {
    /// Load the cached salts. A cache that cannot be read or created is an empty, stale one:
    /// requests ship without a visitor until Cloud's next salts, and crash reports ship regardless.
    pub(super) fn open(store: &StoreRoot, now: i64) -> Self {
        let path = store.salts();
        let cache = match read(&path) {
            Ok(cache) => cache,
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "ignoring an unreadable salt cache");
                Cache::default()
            }
        };
        let salts = Self {
            path,
            cache: RwLock::new(cache),
        };
        if let Err(error) = salts.prune(now) {
            tracing::warn!(%error, "cannot forget closed salts");
        }
        salts
    }

    /// Replace the cache with the sites Cloud lists. A site that does not decode is skipped, so
    /// the Machine stops hashing for it until Cloud sends it whole.
    pub(super) fn accept(&self, body: &[u8], now: i64) {
        let Ok(ack) = serde_json::from_slice::<Ack>(body) else {
            return;
        };
        let mut sites = HashMap::new();
        for site in ack.sites {
            match serde_json::from_value::<Site>(site) {
                Ok(site) => {
                    sites.insert(site.namespace, site.salts);
                }
                Err(error) => tracing::warn!(%error, "skipping a site whose salts do not decode"),
            }
        }
        prune(&mut sites, now);
        let mut cache = self.cache.write().unwrap_or_else(PoisonError::into_inner);
        *cache = Cache {
            fetched: Some(now),
            sites,
        };
        if let Err(error) = self.persist(&cache) {
            tracing::warn!(%error, "cannot keep the salts Cloud sent on disk; using them from memory");
        }
    }

    /// Drop the salts whose day has closed, from memory and from disk, so a Machine that cannot
    /// reach Cloud still forgets them on time.
    pub(super) fn prune(&self, now: i64) -> io::Result<()> {
        let mut cache = self.cache.write().unwrap_or_else(PoisonError::into_inner);
        if prune(&mut cache.sites, now) {
            self.persist(&cache)?;
        }
        Ok(())
    }

    fn persist(&self, cache: &Cache) -> io::Result<()> {
        let persisted = Persisted {
            fetched: cache.fetched.unwrap_or_default(),
            sites: cache
                .sites
                .iter()
                .map(|(namespace, salts)| Site {
                    namespace: namespace.clone(),
                    salts: salts.clone(),
                })
                .collect(),
        };
        let json = serde_json::to_vec(&persisted).map_err(io::Error::other)?;
        crate::filesystem::atomic_write(&self.path, &json, 0o600)
    }

    pub(super) fn stale(&self, now: i64) -> bool {
        let fresh_for = i64::try_from(FRESH_FOR.as_secs()).unwrap_or(i64::MAX);
        self.cache
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .fetched
            .is_none_or(|fetched| now.saturating_sub(fetched) >= fresh_for)
    }

    pub(super) fn lookup(&self, namespace: &Namespace, at: i64) -> Lookup {
        let cache = self.cache.read().unwrap_or_else(PoisonError::into_inner);
        match cache.sites.get(namespace) {
            None => Lookup::Off,
            Some(salts) => salts
                .iter()
                .find(|salt| salt.start <= at && at < salt.end)
                .map_or(Lookup::Gap, |salt| Lookup::Covered {
                    day: salt.day.clone(),
                    salt: salt.salt,
                }),
        }
    }
}

fn read(path: &Path) -> io::Result<Cache> {
    if let Some(parent) = path.parent() {
        create_store_dir(parent)?;
    }
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Cache::default()),
        Err(error) => return Err(error),
    };
    let persisted = serde_json::from_slice::<Persisted>(&bytes).map_err(io::Error::other)?;
    Ok(Cache {
        fetched: Some(persisted.fetched),
        sites: persisted
            .sites
            .into_iter()
            .map(|site| (site.namespace, site.salts))
            .collect(),
    })
}

/// Remove closed salts, keeping every site so one with no open salt still reads as on. True when
/// any salt went.
fn prune(sites: &mut HashMap<Namespace, Vec<Salt>>, now: i64) -> bool {
    let mut pruned = false;
    for salts in sites.values_mut() {
        let before = salts.len();
        salts.retain(|salt| salt.end > now);
        pruned |= salts.len() != before;
    }
    pruned
}

fn day<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    let day = String::deserialize(deserializer)?;
    chrono::NaiveDate::parse_from_str(&day, "%Y-%m-%d").map_err(D::Error::custom)?;
    Ok(day)
}

mod salt_bytes {
    use super::{Deserialize, Deserializer, Engine, SALT_BYTES, Serializer, URL_SAFE_NO_PAD};
    use serde::de::Error as _;

    pub fn serialize<S: Serializer>(
        salt: &[u8; SALT_BYTES],
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&URL_SAFE_NO_PAD.encode(salt))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<[u8; SALT_BYTES], D::Error> {
        let text = String::deserialize(deserializer)?;
        URL_SAFE_NO_PAD
            .decode(text)
            .map_err(D::Error::custom)?
            .try_into()
            .map_err(|_| D::Error::custom("a salt is 32 bytes"))
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;
    use crate::test_dir::TestDir;

    const SALT: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8";

    fn store() -> (TestDir, StoreRoot) {
        let root = TestDir::new("ployzd-observe-salts");
        fs::create_dir_all(&root.0).unwrap();
        let store = StoreRoot::under(&root.0);
        store.prepare().unwrap();
        (root, store)
    }

    fn ack(windows: &[(&str, i64, i64)]) -> Vec<u8> {
        let salts: Vec<_> = windows
            .iter()
            .map(|(day, start, end)| {
                serde_json::json!({ "day": day, "start": start, "end": end, "salt": SALT })
            })
            .collect();
        serde_json::to_vec(&serde_json::json!({
            "sites": [{ "namespace": "acme-prod", "salts": salts }],
        }))
        .unwrap()
    }

    fn acme() -> Namespace {
        Namespace::parse("acme-prod").unwrap()
    }

    fn day_at(salts: &Salts, namespace: &Namespace, at: i64) -> Option<Option<String>> {
        match salts.lookup(namespace, at) {
            Lookup::Off => None,
            Lookup::Gap => Some(None),
            Lookup::Covered { day, .. } => Some(Some(day)),
        }
    }

    #[test]
    fn an_ack_replaces_the_cache_prunes_closed_salts_and_survives_a_restart() {
        let (_root, store) = store();
        let salts = Salts::open(&store, 0);
        assert!(salts.stale(0));
        salts.accept(
            &ack(&[("2026-10-14", 0, 100), ("2026-10-15", 100, 200)]),
            150,
        );
        assert!(!salts.stale(150));
        assert_eq!(day_at(&salts, &acme(), 50), Some(None));
        assert_eq!(
            day_at(&salts, &acme(), 150),
            Some(Some("2026-10-15".into()))
        );

        let mode = fs::metadata(store.salts()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let reopened = Salts::open(&store, 160);
        assert!(!reopened.stale(160));
        assert_eq!(
            day_at(&reopened, &acme(), 199),
            Some(Some("2026-10-15".into()))
        );

        reopened.accept(br#"{"sites":[]}"#, 170);
        assert_eq!(day_at(&reopened, &acme(), 199), None);
    }

    #[test]
    fn a_body_that_does_not_decode_leaves_the_cache_untouched() {
        let (_root, store) = store();
        let salts = Salts::open(&store, 0);
        salts.accept(&ack(&[("2026-10-15", 0, 100)]), 10);
        for body in [&b""[..], b"not json", br#"{"sites":{}}"#] {
            salts.accept(body, 20);
        }
        assert_eq!(day_at(&salts, &acme(), 50), Some(Some("2026-10-15".into())));
        assert!(salts.stale(10 + 3600));
    }

    #[test]
    fn a_site_that_does_not_decode_is_skipped_and_the_rest_are_kept() {
        let (_root, store) = store();
        let salts = Salts::open(&store, 0);
        let body = serde_json::json!({ "sites": [
            { "namespace": "short", "salts": [{ "day": "2026-10-15", "start": 0, "end": 100, "salt": "c2hvcnQ" }] },
            { "namespace": "Not_A_Label", "salts": [] },
            { "namespace": "bad-day", "salts": [{ "day": "yesterday", "start": 0, "end": 100, "salt": SALT }] },
            { "namespace": "acme-prod", "salts": [{ "day": "2026-10-15", "start": 0, "end": 100, "salt": SALT }] },
            { "namespace": "quiet", "salts": [] },
        ] });
        salts.accept(&serde_json::to_vec(&body).unwrap(), 10);
        assert_eq!(day_at(&salts, &acme(), 50), Some(Some("2026-10-15".into())));
        for skipped in ["short", "bad-day"] {
            assert_eq!(
                day_at(&salts, &Namespace::parse(skipped).unwrap(), 50),
                None
            );
        }
        assert_eq!(
            day_at(&salts, &Namespace::parse("quiet").unwrap(), 50),
            Some(None),
            "a site with no salts yet is on, so its views count as a gap"
        );
        assert!(!salts.stale(10));
    }

    #[test]
    fn closed_salts_are_forgotten_on_the_clock_without_cloud() {
        let (_root, store) = store();
        let salts = Salts::open(&store, 0);
        salts.accept(
            &ack(&[("2026-10-14", 0, 100), ("2026-10-15", 100, 200)]),
            50,
        );
        let on_disk = || String::from_utf8(fs::read(store.salts()).unwrap()).unwrap();
        assert!(on_disk().contains("2026-10-14"));

        salts.prune(99).unwrap();
        assert!(on_disk().contains("2026-10-14"));
        salts.prune(100).unwrap();
        assert!(!on_disk().contains("2026-10-14"));
        assert_eq!(day_at(&salts, &acme(), 50), Some(None));

        salts.prune(200).unwrap();
        assert_eq!(
            day_at(&salts, &acme(), 150),
            Some(None),
            "the site stays on"
        );
        assert!(!on_disk().contains("2026-10-15"));
        assert!(salts.stale(50 + 3600), "pruning is not a fetch");
    }

    #[test]
    fn startup_forgets_salts_that_closed_while_the_daemon_was_down() {
        let (_root, store) = store();
        Salts::open(&store, 0).accept(
            &ack(&[("2026-10-14", 0, 100), ("2026-10-15", 100, 200)]),
            50,
        );
        let reopened = Salts::open(&store, 150);
        let on_disk = String::from_utf8(fs::read(store.salts()).unwrap()).unwrap();
        assert!(!on_disk.contains("2026-10-14"), "{on_disk}");
        assert!(on_disk.contains("2026-10-15"), "{on_disk}");
        assert_eq!(
            day_at(&reopened, &acme(), 150),
            Some(Some("2026-10-15".into()))
        );
        assert!(!reopened.stale(50));
    }

    #[test]
    fn a_site_cloud_does_not_list_is_off() {
        let (_root, store) = store();
        let salts = Salts::open(&store, 0);
        salts.accept(&ack(&[("2026-10-15", 0, 100)]), 10);
        assert_eq!(
            day_at(&salts, &Namespace::parse("other").unwrap(), 50),
            None
        );
    }

    #[test]
    fn a_cache_directory_that_cannot_be_made_still_serves_the_salts_cloud_sends() {
        let (_root, store) = store();
        fs::write(store.salts().parent().unwrap(), b"in the way").unwrap();
        let salts = Salts::open(&store, 0);
        assert!(salts.stale(0));
        assert_eq!(day_at(&salts, &acme(), 0), None);
        salts.accept(&ack(&[("2026-10-15", 0, 100)]), 10);
        assert!(!salts.stale(10));
        assert_eq!(day_at(&salts, &acme(), 50), Some(Some("2026-10-15".into())));
    }

    #[test]
    fn an_unreadable_cache_file_is_an_empty_stale_cache() {
        let (_root, store) = store();
        create_store_dir(store.salts().parent().unwrap()).unwrap();
        fs::write(store.salts(), b"{").unwrap();
        let salts = Salts::open(&store, 0);
        assert!(salts.stale(0));
        assert_eq!(day_at(&salts, &acme(), 0), None);
    }
}
