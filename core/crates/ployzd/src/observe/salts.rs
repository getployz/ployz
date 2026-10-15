use std::{
    collections::HashMap,
    fs, io,
    path::PathBuf,
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
pub(super) struct Ack {
    sites: Vec<Site>,
}

#[derive(Debug, Deserialize, Serialize)]
struct Persisted {
    fetched: i64,
    sites: Vec<Site>,
}

#[derive(Debug, PartialEq)]
pub(super) enum Lookup<'a> {
    Off,
    Gap,
    Covered(&'a Salt),
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
    pub(super) fn open(store: &StoreRoot, now: i64) -> io::Result<Self> {
        let path = store.salts();
        if let Some(parent) = path.parent() {
            create_store_dir(parent)?;
        }
        let cache = match fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<Persisted>(&bytes) {
                Ok(persisted) => Cache {
                    fetched: Some(persisted.fetched),
                    sites: open_sites(persisted.sites, now),
                },
                Err(error) => {
                    tracing::warn!(path = %path.display(), %error, "ignoring an unreadable salt cache");
                    Cache::default()
                }
            },
            Err(error) if error.kind() == io::ErrorKind::NotFound => Cache::default(),
            Err(error) => return Err(error),
        };
        Ok(Self {
            path,
            cache: RwLock::new(cache),
        })
    }

    pub(super) fn accept(&self, body: &[u8], now: i64) -> io::Result<()> {
        let Ok(ack) = serde_json::from_slice::<Ack>(body) else {
            return Ok(());
        };
        let sites = open_sites(ack.sites, now);
        let persisted = Persisted {
            fetched: now,
            sites: sites
                .iter()
                .map(|(namespace, salts)| Site {
                    namespace: namespace.clone(),
                    salts: salts.clone(),
                })
                .collect(),
        };
        let json = serde_json::to_vec(&persisted).map_err(io::Error::other)?;
        let mut cache = self.cache.write().unwrap_or_else(PoisonError::into_inner);
        crate::filesystem::atomic_write(&self.path, &json, 0o600)?;
        *cache = Cache {
            fetched: Some(now),
            sites,
        };
        Ok(())
    }

    pub(super) fn stale(&self, now: i64) -> bool {
        let fresh_for = i64::try_from(FRESH_FOR.as_secs()).unwrap_or(i64::MAX);
        self.cache
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .fetched
            .is_none_or(|fetched| now.saturating_sub(fetched) >= fresh_for)
    }

    pub(super) fn lookup<R>(
        &self,
        namespace: &Namespace,
        at: i64,
        with: impl FnOnce(Lookup<'_>) -> R,
    ) -> R {
        let cache = self.cache.read().unwrap_or_else(PoisonError::into_inner);
        let lookup = match cache.sites.get(namespace) {
            None => Lookup::Off,
            Some(salts) => salts
                .iter()
                .find(|salt| salt.start <= at && at < salt.end)
                .map_or(Lookup::Gap, Lookup::Covered),
        };
        with(lookup)
    }
}

fn open_sites(sites: Vec<Site>, now: i64) -> HashMap<Namespace, Vec<Salt>> {
    sites
        .into_iter()
        .map(|site| {
            let salts = site.salts.into_iter().filter(|salt| salt.end > now);
            (site.namespace, salts.collect())
        })
        .collect()
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
        salts.lookup(namespace, at, |lookup| match lookup {
            Lookup::Off => None,
            Lookup::Gap => Some(None),
            Lookup::Covered(salt) => Some(Some(salt.day.clone())),
        })
    }

    #[test]
    fn an_ack_replaces_the_cache_prunes_closed_salts_and_survives_a_restart() {
        let (_root, store) = store();
        let salts = Salts::open(&store, 0).unwrap();
        assert!(salts.stale(0));
        salts
            .accept(
                &ack(&[("2026-10-14", 0, 100), ("2026-10-15", 100, 200)]),
                150,
            )
            .unwrap();
        assert!(!salts.stale(150));
        assert_eq!(day_at(&salts, &acme(), 50), Some(None));
        assert_eq!(
            day_at(&salts, &acme(), 150),
            Some(Some("2026-10-15".into()))
        );

        let mode = fs::metadata(store.salts()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let reopened = Salts::open(&store, 160).unwrap();
        assert!(!reopened.stale(160));
        assert_eq!(
            day_at(&reopened, &acme(), 199),
            Some(Some("2026-10-15".into()))
        );

        reopened.accept(br#"{"sites":[]}"#, 170).unwrap();
        assert_eq!(day_at(&reopened, &acme(), 199), None);
    }

    #[test]
    fn a_body_that_does_not_decode_leaves_the_cache_untouched() {
        let (_root, store) = store();
        let salts = Salts::open(&store, 0).unwrap();
        salts.accept(&ack(&[("2026-10-15", 0, 100)]), 10).unwrap();
        for body in [
            &b""[..],
            b"not json",
            br#"{"sites":[{"namespace":"acme-prod","salts":[{"day":"2026-10-15","start":0,"end":100,"salt":"c2hvcnQ"}]}]}"#,
            br#"{"sites":[{"namespace":"Not_A_Label","salts":[]}]}"#,
            br#"{"sites":[{"namespace":"acme-prod","salts":[{"day":"yesterday","start":0,"end":100,"salt":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8"}]}]}"#,
        ] {
            salts.accept(body, 20).unwrap();
        }
        assert_eq!(day_at(&salts, &acme(), 50), Some(Some("2026-10-15".into())));
        assert!(salts.stale(10 + 3600));
    }

    #[test]
    fn a_site_cloud_does_not_list_is_off() {
        let (_root, store) = store();
        let salts = Salts::open(&store, 0).unwrap();
        salts.accept(&ack(&[("2026-10-15", 0, 100)]), 10).unwrap();
        assert_eq!(
            day_at(&salts, &Namespace::parse("other").unwrap(), 50),
            None
        );
    }

    #[test]
    fn an_unreadable_cache_file_is_an_empty_stale_cache() {
        let (_root, store) = store();
        create_store_dir(store.salts().parent().unwrap()).unwrap();
        fs::write(store.salts(), b"{").unwrap();
        let salts = Salts::open(&store, 0).unwrap();
        assert!(salts.stale(0));
        assert_eq!(day_at(&salts, &acme(), 0), None);
    }
}
