//! Regions. A Server's region is its `region` Machine Label, whose value is the region's
//! key, and placement reads only that label. A region's name is what people see: the
//! Organization keeps it here, and nothing that places reads it, so a rename restarts nothing.

use std::collections::BTreeMap;
use std::fmt;

use ployz_core::RpcError;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use ts_rs::TS;

use crate::Actor;
use crate::error;
use crate::storage::Tx;

/// The Machine Label that holds a Server's region.
pub const REGION_LABEL: &str = "region";

const KEY_RULE: &str =
    "Use up to 63 lowercase letters, digits and hyphens. Start with a letter or digit.";
const NAME_RULE: &str = "A region's name is 1 to 40 characters on one line.";
const DERIVED_RULE: &str = "Use at least one letter a-z or digit.";

/// A region's key: a Server's `region` label value in lowercase, as placement compares it.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize, TS)]
#[serde(try_from = "String", into = "String")]
#[ts(as = "String")]
pub struct RegionKey(String);

impl RegionKey {
    /// `value` in lowercase, when that is a key.
    ///
    /// # Errors
    /// Returns `invalid_argument` saying what a key is.
    pub fn parse(value: impl Into<String>) -> Result<Self, RpcError> {
        let value = value.into().to_ascii_lowercase();
        let mut bytes = value.bytes();
        let valid = value.len() <= 63
            && bytes
                .next()
                .is_some_and(|first| first.is_ascii_lowercase() || first.is_ascii_digit())
            && bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
        if valid {
            Ok(Self(value))
        } else {
            Err(error::invalid(KEY_RULE, Value::Null))
        }
    }

    /// Where a Server starts when it joins with no region.
    #[must_use]
    pub fn default_region() -> Self {
        Self("default".to_owned())
    }

    /// The key a new region named `name` takes: its letters and digits in lowercase, each
    /// run of anything else one hyphen, with no hyphen at either end. A name's 40
    /// characters keep it within a key's 63.
    ///
    /// # Errors
    /// Returns `invalid_argument` when `name` has no letter a-z or digit to make it from.
    pub fn from_name(name: &RegionName) -> Result<Self, RpcError> {
        let mut key = String::new();
        for character in name.as_str().chars().map(|c| c.to_ascii_lowercase()) {
            if character.is_ascii_lowercase() || character.is_ascii_digit() {
                key.push(character);
            } else if !key.ends_with('-') {
                key.push('-');
            }
        }
        let key = key.trim_matches('-');
        if key.is_empty() {
            return Err(error::invalid(DERIVED_RULE, Value::Null));
        }
        Self::parse(key)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RegionKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl TryFrom<String> for RegionKey {
    type Error = RpcError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

impl From<RegionKey> for String {
    fn from(value: RegionKey) -> Self {
        value.0
    }
}

/// A name a region was given: one line of 1 to 40 characters, trimmed.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize, TS)]
#[serde(try_from = "String", into = "String")]
#[ts(as = "String")]
pub struct RegionName(String);

impl RegionName {
    /// `value` trimmed, when that is a name.
    ///
    /// # Errors
    /// Returns `invalid_argument` saying what a name is.
    pub fn parse(value: impl Into<String>) -> Result<Self, RpcError> {
        let value = value.into();
        let value = value.trim();
        if (1..=40).contains(&value.chars().count()) && !value.chars().any(char::is_control) {
            Ok(Self(value.to_owned()))
        } else {
            Err(error::invalid(NAME_RULE, Value::Null))
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RegionName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl TryFrom<String> for RegionName {
    type Error = RpcError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

impl From<RegionName> for String {
    fn from(value: RegionName) -> Self {
        value.0
    }
}

/// The name region `key` shows as: the one it was given, else `Default` for the default
/// region and the key itself for any other.
#[must_use]
pub(crate) fn shown(key: &RegionKey, given: Option<&RegionName>) -> String {
    match given {
        Some(name) => name.to_string(),
        None if *key == RegionKey::default_region() => "Default".to_owned(),
        None => key.to_string(),
    }
}

/// One region: its key, and the name it shows as.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct Region {
    pub key: RegionKey,
    /// The name it was given, else its built-in one (see [`shown`]).
    pub name: String,
}

/// The regions a Server's label holds or a name was given to, by key. `labels` are the
/// Servers' `region` label values, in any case; one that is no key holds no region.
#[must_use]
pub(crate) fn regions<'a>(
    labels: impl IntoIterator<Item = &'a str>,
    given: &BTreeMap<RegionKey, RegionName>,
) -> Vec<Region> {
    let mut keys: Vec<RegionKey> = labels
        .into_iter()
        .filter_map(|label| RegionKey::parse(label).ok())
        .chain(given.keys().cloned())
        .collect();
    keys.sort();
    keys.dedup();
    keys.into_iter()
        .map(|key| Region {
            name: shown(&key, given.get(&key)),
            key,
        })
        .collect()
}

/// A region and the Servers in it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RegionServers<'a> {
    pub name: String,
    pub key: RegionKey,
    pub servers: Vec<&'a str>,
}

/// Each region with the Servers in it, and the Servers in none.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ServersByRegion<'a> {
    pub regions: Vec<RegionServers<'a>>,
    pub no_region: Vec<&'a str>,
}

/// Each of `regions` with the `servers` in it, given as each Server's name and its `region`
/// label's value in any case. A Server whose label is missing or no key is in no region.
#[must_use]
pub fn region_servers<'a>(
    regions: Vec<Region>,
    servers: &[(&'a str, Option<&str>)],
) -> ServersByRegion<'a> {
    let keyed = servers
        .iter()
        .map(|(server, label)| {
            (
                *server,
                label.and_then(|label| RegionKey::parse(label).ok()),
            )
        })
        .collect::<Vec<_>>();
    ServersByRegion {
        regions: regions
            .into_iter()
            .map(|region| RegionServers {
                servers: keyed
                    .iter()
                    .filter(|(_, key)| key.as_ref() == Some(&region.key))
                    .map(|(server, _)| *server)
                    .collect(),
                name: region.name,
                key: region.key,
            })
            .collect(),
        no_region: keyed
            .iter()
            .filter(|(_, key)| key.is_none())
            .map(|(server, _)| *server)
            .collect(),
    }
}

/// The key a new region named `name` takes among the `known` regions, refused when one of
/// them has that key or shows that name, in any case.
///
/// # Errors
/// Returns `invalid_argument` when `name` makes no key, or `conflict` naming the region
/// that has the key or the name.
pub(crate) fn new_region(name: &RegionName, known: &[Region]) -> Result<RegionKey, RpcError> {
    let key = RegionKey::from_name(name)?;
    let named = || {
        known
            .iter()
            .find(|region| region.name.to_lowercase() == name.as_str().to_lowercase())
    };
    match known.iter().find(|region| region.key == key).or_else(named) {
        Some(region) => {
            let existing = if region.name == region.key.as_str() {
                region.name.clone()
            } else {
                format!("{} ({})", region.name, region.key)
            };
            Err(error::conflict(
                format!("Region {existing} already exists."),
                json!({ "region": region }),
            ))
        }
        None => Ok(key),
    }
}

/// Ask for the Organization's regions: each key in `labels` or with a name, and its name.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct RegionsQuery {
    /// The `region` label value of each Server the caller sees.
    #[serde(default)]
    pub labels: Vec<String>,
}

/// The Organization's regions, by key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct RegionsView {
    pub regions: Vec<Region>,
}

/// Ask for the key a new region named `name` takes, refused when a known region already has
/// it in any case.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct NewRegionQuery {
    pub name: RegionName,
    /// The `region` label value of each Server the caller sees.
    #[serde(default)]
    pub labels: Vec<String>,
}

/// The key a new region takes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct NewRegionView {
    pub key: RegionKey,
}

/// Give a region a name. Its key stays, and so does every Server's label and every
/// Environment, so nothing restarts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct NameRegion {
    pub key: RegionKey,
    pub name: RegionName,
}

pub(crate) fn view(
    tx: &mut dyn Tx,
    who: &Actor,
    query: &RegionsQuery,
) -> Result<RegionsView, RpcError> {
    let given = given(tx, who)?;
    Ok(RegionsView {
        regions: regions(query.labels.iter().map(String::as_str), &given),
    })
}

pub(crate) fn new_view(
    tx: &mut dyn Tx,
    who: &Actor,
    query: &NewRegionQuery,
) -> Result<NewRegionView, RpcError> {
    let known = regions(query.labels.iter().map(String::as_str), &given(tx, who)?);
    Ok(NewRegionView {
        key: new_region(&query.name, &known)?,
    })
}

pub(crate) fn name(tx: &mut dyn Tx, who: &Actor, named: &NameRegion) -> Result<Region, RpcError> {
    tx.execute(
        "INSERT INTO config_region_name (organization_id, key, name) VALUES (?1, ?2, ?3) \
         ON CONFLICT (organization_id, key) DO UPDATE SET name = excluded.name",
        &[
            who.organization.as_str().into(),
            named.key.as_str().into(),
            named.name.as_str().into(),
        ],
    )?;
    Ok(Region {
        key: named.key.clone(),
        name: named.name.to_string(),
    })
}

fn given(tx: &mut dyn Tx, who: &Actor) -> Result<BTreeMap<RegionKey, RegionName>, RpcError> {
    tx.query(
        "SELECT key, name FROM config_region_name WHERE organization_id = ?1",
        &[who.organization.as_str().into()],
    )?
    .iter()
    .map(|row| Ok((row.parse(0, "Region")?, row.parse(1, "Region name")?)))
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(text: &str) -> RegionName {
        RegionName::parse(text).unwrap()
    }

    fn key(text: &str) -> RegionKey {
        RegionKey::parse(text).unwrap()
    }

    #[test]
    fn a_region_key_is_lowercase_and_fits_the_label_grammar() {
        assert_eq!(key("FSN").as_str(), "fsn");
        assert_eq!(key("eu-west-1").as_str(), "eu-west-1");
        assert_eq!(key(&"a".repeat(63)).as_str().len(), 63);
        for refused in ["", "-fsn", "eu_west", "eu west", "zürich", &"a".repeat(64)] {
            assert_eq!(
                RegionKey::parse(refused).unwrap_err().message,
                KEY_RULE,
                "{refused:?}"
            );
        }
    }

    #[test]
    fn a_region_name_is_one_trimmed_line_of_up_to_forty_characters() {
        assert_eq!(name("  Falkenstein ").as_str(), "Falkenstein");
        assert_eq!(name(&"é".repeat(40)).as_str().chars().count(), 40);
        for refused in ["", "   ", "Falk\nenstein", &"a".repeat(41)] {
            assert_eq!(
                RegionName::parse(refused).unwrap_err().message,
                NAME_RULE,
                "{refused:?}"
            );
        }
    }

    #[test]
    fn a_region_without_a_name_shows_default_or_its_key() {
        assert_eq!(shown(&RegionKey::default_region(), None), "Default");
        assert_eq!(shown(&key("syd"), None), "syd");
        assert_eq!(
            shown(&RegionKey::default_region(), Some(&name("Falkenstein"))),
            "Falkenstein"
        );
    }

    #[test]
    fn a_new_regions_key_comes_from_its_name() {
        let derived = |text: &str| RegionKey::from_name(&name(text)).map(String::from);
        assert_eq!(derived("Sydney").unwrap(), "sydney");
        assert_eq!(derived("US East (Ohio)").unwrap(), "us-east-ohio");
        assert_eq!(derived("--Zürich 2--").unwrap(), "z-rich-2");
        assert_eq!(derived("fra1").unwrap(), "fra1");
        assert_eq!(
            derived(&"Ab-".repeat(13)).unwrap(),
            format!("{}ab", "ab-".repeat(12))
        );
        assert_eq!(
            RegionKey::from_name(&name("東京")).unwrap_err().message,
            DERIVED_RULE
        );
    }

    #[test]
    fn regions_are_the_servers_keys_and_the_named_ones_in_lowercase() {
        let given = BTreeMap::from([
            (RegionKey::default_region(), name("Falkenstein")),
            (key("lhr"), name("London")),
        ]);
        assert_eq!(
            regions(["SYD", "default", "syd", "eu_west"], &given),
            [
                Region {
                    key: RegionKey::default_region(),
                    name: "Falkenstein".into()
                },
                Region {
                    key: key("lhr"),
                    name: "London".into()
                },
                Region {
                    key: key("syd"),
                    name: "syd".into()
                },
            ]
        );
    }

    #[test]
    fn a_region_lists_its_servers_whatever_case_their_label_is_in() {
        let servers = [
            ("web-1", Some("SYD")),
            ("web-2", Some("default")),
            ("db-1", Some("syd")),
            ("old-1", None),
            ("odd-1", Some("eu_west")),
        ];
        let listed = region_servers(
            regions(
                servers.iter().filter_map(|(_, label)| *label),
                &BTreeMap::new(),
            ),
            &servers,
        );
        assert_eq!(
            listed
                .regions
                .iter()
                .map(|region| (region.key.as_str(), region.servers.clone()))
                .collect::<Vec<_>>(),
            [("default", vec!["web-2"]), ("syd", vec!["web-1", "db-1"])]
        );
        assert_eq!(listed.no_region, ["old-1", "odd-1"]);
    }

    #[test]
    fn a_new_region_whose_key_exists_in_another_case_is_refused_naming_it() {
        let known = regions(["FSN"], &BTreeMap::new());
        let refused = new_region(&name("fsn!"), &known).unwrap_err();
        assert_eq!(refused.code, ployz_core::RpcErrorCode::Conflict);
        assert_eq!(refused.message, "Region fsn already exists.");

        let named = regions(
            ["fsn"],
            &BTreeMap::from([(key("fsn"), name("Falkenstein"))]),
        );
        assert_eq!(
            new_region(&name("FSN"), &named).unwrap_err().message,
            "Region Falkenstein (fsn) already exists."
        );
        assert_eq!(new_region(&name("Sydney"), &named).unwrap(), key("sydney"));
    }

    #[test]
    fn a_new_region_named_like_an_existing_one_is_refused_naming_it() {
        let known = regions(
            ["sydney", "default"],
            &BTreeMap::from([(key("sydney"), name("Sydney"))]),
        );
        assert_eq!(
            new_region(&name("Sydney"), &known).unwrap_err().message,
            "Region Sydney (sydney) already exists."
        );
        assert_eq!(
            new_region(&name("Default"), &known).unwrap_err().message,
            "Region Default (default) already exists."
        );
    }

    #[test]
    fn a_new_region_named_like_one_that_shows_that_name_is_refused_in_any_case() {
        let known = regions(
            ["syd", "default"],
            &BTreeMap::from([(key("syd"), name("Sydney"))]),
        );
        assert_eq!(
            new_region(&name("sydney"), &known).unwrap_err().message,
            "Region Sydney (syd) already exists."
        );
        assert_eq!(
            new_region(&name("DEFAULT"), &known).unwrap_err().message,
            "Region Default (default) already exists."
        );
        assert_eq!(
            new_region(&name("Sydney 2"), &known).unwrap(),
            key("sydney-2")
        );
    }
}
