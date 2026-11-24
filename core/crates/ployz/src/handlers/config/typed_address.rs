//! What an edit says of its Typed Addresses, to a person and under `--json`: the
//! reference to set instead, built from what was typed, so it may repeat a secret only
//! through [`Reference`], which withholds every word that may hold one.

use clap::ArgMatches;
use ployz_core::ServiceName;
use ployz_store::{Edited, EnvironmentSummary, Instead, SettingNote, SettingPath, TypedAddresses};
use serde::Serialize;

use crate::cli::Rerun;
use crate::ui;

/// An edit's result as `--json` prints it: each Typed Address's reference withheld.
#[derive(Serialize)]
pub(super) struct Shown<'a> {
    environment: &'a EnvironmentSummary,
    staged: &'a [SettingPath],
    immediate: &'a [SettingPath],
    typed_addresses: Vec<ShownTyped<'a>>,
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    notes: &'a [SettingNote],
}

impl<'a> Shown<'a> {
    pub(super) fn of(edited: &'a Edited) -> Self {
        Self {
            environment: &edited.environment,
            staged: &edited.staged,
            immediate: &edited.immediate,
            typed_addresses: edited
                .typed_addresses
                .iter()
                .map(|typed| ShownTyped {
                    path: &typed.path,
                    services: &typed.services,
                    instead: Reference::of(typed),
                })
                .collect(),
            notes: &edited.notes,
        }
    }
}

#[derive(Serialize)]
struct ShownTyped<'a> {
    path: &'a SettingPath,
    services: &'a [ServiceName],
    instead: Reference,
}

/// The value to set instead of one with Typed Addresses, as Ployz shows it: each address
/// referenced, and each word that may hold a secret as the placeholder it lists in
/// `withheld`. The rest of the value was typed too, but is a URL's scheme, host, port or
/// path, a reference, or `KEY=` before a reference.
#[derive(Serialize)]
#[serde(tag = "kind", rename = "reference")]
pub(super) struct Reference {
    value: String,
    /// The placeholders `value` holds, to type in again; none when it is ready to set.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    withheld: Vec<&'static str>,
}

const USER: &str = "USER";
const PASSWORD: &str = "PASSWORD";
const QUERY: &str = "QUERY";
const FRAGMENT: &str = "FRAGMENT";
const VALUE: &str = "VALUE";

/// Around the index of a reference while words are read, so none splits inside one.
const MARK: char = '\0';

impl Reference {
    fn of(typed: &TypedAddresses) -> Self {
        let Instead::Reference { value } = &typed.instead;
        Self::of_value(value)
    }

    fn of_value(value: &str) -> Self {
        let (text, references) = marked(value);
        let mut withheld = Vec::new();
        let mut shown = String::with_capacity(text.len());
        let mut word = String::new();
        for c in text.chars() {
            if c.is_whitespace() || matches!(c, ';' | ',') {
                shown.push_str(&shown_word(&word, &mut withheld));
                shown.push(c);
                word.clear();
            } else {
                word.push(c);
            }
        }
        shown.push_str(&shown_word(&word, &mut withheld));
        // Between marks, every other piece is the index of a reference a word kept.
        let value = shown
            .split(MARK)
            .enumerate()
            .map(|(at, piece)| match at % 2 {
                0 => piece,
                _ => piece
                    .parse::<usize>()
                    .ok()
                    .and_then(|index| references.get(index).copied())
                    .unwrap_or_default(),
            })
            .collect();
        Self { value, withheld }
    }
}

/// `value` with each `${{ … }}` its index between [`MARK`]s, and the references in order.
fn marked(value: &str) -> (String, Vec<&str>) {
    let mut text = String::with_capacity(value.len());
    let mut references = Vec::new();
    let mut rest = value;
    while let Some(start) = rest.find("${{") {
        let Some(end) = rest.get(start..).and_then(|from| from.find("}}")) else {
            break;
        };
        let end = start + end + 2;
        text.push_str(rest.get(..start).unwrap_or_default());
        text.push(MARK);
        text.push_str(&references.len().to_string());
        text.push(MARK);
        references.extend(rest.get(start..end));
        rest = rest.get(end..).unwrap_or_default();
    }
    text.push_str(rest);
    // Text never holds a NUL (the Store refuses one), so each is a reference.
    (text, references)
}

/// One word as shown: a URL without its user info, query or fragment, `KEY=` before an
/// address, or an address; anything else is `VALUE`.
fn shown_word(word: &str, withheld: &mut Vec<&'static str>) -> String {
    let mut withhold = |placeholder: &'static str| {
        if !withheld.contains(&placeholder) {
            withheld.push(placeholder);
        }
        placeholder
    };
    if word.is_empty() || address(word) {
        return word.to_owned();
    }
    if let Some((scheme, rest)) = word.split_once("://") {
        let (before_fragment, fragment) = match rest.split_once('#') {
            Some((before, _)) => (before, Some(withhold(FRAGMENT))),
            None => (rest, None),
        };
        let (before_query, query) = match before_fragment.split_once('?') {
            Some((before, _)) => (before, Some(withhold(QUERY))),
            None => (before_fragment, None),
        };
        let (authority, path) = before_query
            .find('/')
            .map_or((before_query, ""), |slash| before_query.split_at(slash));
        let host = match authority.rsplit_once('@') {
            Some((userinfo, host)) => {
                let user = withhold(USER);
                match userinfo.contains(':') {
                    true => format!("{user}:{}@{host}", withhold(PASSWORD)),
                    false => format!("{user}@{host}"),
                }
            }
            None => authority.to_owned(),
        };
        let query = query.map(|query| format!("?{query}")).unwrap_or_default();
        let fragment = fragment.map(|it| format!("#{it}")).unwrap_or_default();
        return format!("{scheme}://{host}{path}{query}{fragment}");
    }
    match word.split_once('=') {
        Some((key, value)) if address(value) => format!("{key}={value}"),
        Some(_) | None => withhold(VALUE).to_owned(),
    }
}

/// A reference, with a port after it if any.
fn address(word: &str) -> bool {
    let reference = word
        .strip_prefix(MARK)
        .and_then(|marked| marked.split_once(MARK))
        .filter(|(index, _)| !index.is_empty() && index.bytes().all(|b| b.is_ascii_digit()));
    reference.is_some_and(|(_, port)| {
        port.is_empty()
            || port
                .strip_prefix(':')
                .is_some_and(|port| !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()))
    })
}

/// What a variable's Typed Addresses cost, and what to set instead in `environment`.
pub(super) fn say(matches: &ArgMatches, environment: &EnvironmentSummary, typed: &TypedAddresses) {
    let (path, services) = (&typed.path, super::super::joined(&typed.services));
    let them = if typed.services.len() == 1 {
        "it"
    } else {
        "them"
    };
    let Some(consumer) = path.node() else {
        return;
    };
    ui::warn(format!(
        "{path} types the private address of {services}, so Ployz can't see that {consumer} uses {them}: a Branch that doesn't copy {them} can't reach {them}, and a Deploy won't start {them} first."
    ));
    let scope = super::super::store::scope_args(&environment.project, &environment.name);
    match set_instead(matches, path, &Reference::of(typed), &scope) {
        Ok(retry) => ui::hint(&retry.into()),
        Err(by_hand) => ui::note(by_hand),
    }
}

/// `set` of `path` to `reference` in `scope`; when it withholds a word, the sentence to
/// set it by hand, naming each placeholder to type over.
fn set_instead(
    matches: &ArgMatches,
    path: &SettingPath,
    reference: &Reference,
    scope: &[&str],
) -> Result<Rerun, String> {
    let assignment = format!("{path}={}", reference.value);
    if let Some((last, first)) = reference.withheld.split_last() {
        let named = match first {
            [] => (*last).to_owned(),
            _ => format!("{} and {last}", first.join(", ")),
        };
        return Err(format!(
            "Set the reference instead, typing what you typed where it says {named}: {assignment}"
        ));
    }
    // Parsed, the assignment shows as VALUE: a value typed may be a secret. Reference
    // withheld every word that may hold one.
    Ok(Rerun::then(
        matches,
        &[&["set", assignment.as_str()][..], scope].concat(),
    )
    .set("assignment", &assignment))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shown(value: &str) -> (String, Vec<&'static str>) {
        let reference = Reference::of_value(value);
        (reference.value, reference.withheld)
    }

    const DB: &str = "${{ db.PLOYZ_PRIVATE_DOMAIN }}";
    const CACHE: &str = "${{ cache.PLOYZ_PRIVATE_DOMAIN }}";

    /// A reference keeps what locates the Service and withholds each word a secret may
    /// ride in: a URL's user info, query and fragment, and any other word.
    #[test]
    fn a_reference_withholds_every_word_a_secret_may_ride_in() {
        for (value, expected, withheld) in [
            (
                format!("http://{DB}:8080/api"),
                format!("http://{DB}:8080/api"),
                &[][..],
            ),
            (format!("{DB}:6379"), format!("{DB}:6379"), &[]),
            (
                format!("postgres://app:EXAMPLE@{DB}:5432/shop"),
                format!("postgres://USER:PASSWORD@{DB}:5432/shop"),
                &[USER, PASSWORD],
            ),
            (
                format!("https://EXAMPLE@{DB}/repo.git"),
                format!("https://USER@{DB}/repo.git"),
                &[USER],
            ),
            (
                format!("http://{DB}/?token=EXAMPLE#EXAMPLE"),
                format!("http://{DB}/?QUERY#FRAGMENT"),
                &[FRAGMENT, QUERY],
            ),
            (
                format!("host={DB} port=5432 password=EXAMPLE"),
                format!("host={DB} VALUE VALUE"),
                &[VALUE],
            ),
            (
                format!("{DB}:6379,password=EXAMPLE"),
                format!("{DB}:6379,VALUE"),
                &[VALUE],
            ),
            (
                format!("Server={DB};Password=EXAMPLE"),
                format!("Server={DB};VALUE"),
                &[VALUE],
            ),
            (format!("{DB}:EXAMPLE"), "VALUE".to_owned(), &[VALUE]),
            (
                format!("http://{DB}/?key=${{{{ shared.KEY }}}} {CACHE}:6379"),
                format!("http://{DB}/?QUERY {CACHE}:6379"),
                &[QUERY],
            ),
        ] {
            assert_eq!(shown(&value), (expected, withheld.to_vec()), "{value}");
        }
    }

    /// The line that sets the reference runs as shown only when nothing was withheld;
    /// otherwise the reader types each placeholder over, and no secret reaches the hint.
    #[test]
    fn a_typed_address_names_the_set_of_its_reference_without_any_secret() {
        let root = crate::cli::command()
            .try_get_matches_from(["ployz", "set", "web.env.API_URL=http://db.internal:8080"])
            .unwrap();
        let scope = ["--project", "shop", "--env", "staging"];
        let path = |text| SettingPath::parse(text).unwrap();
        let retry = set_instead(
            &root,
            &path("web.env.API_URL"),
            &Reference::of_value(&format!("http://{DB}:8080")),
            &scope,
        )
        .unwrap();
        assert_eq!(
            retry.line(),
            "ployz set 'web.env.API_URL=http://${{ db.PLOYZ_PRIVATE_DOMAIN }}:8080' \
             --project shop --env staging"
        );
        for (value, by_hand) in [
            (
                format!("postgres://app:EXAMPLE@{DB}:5432/shop"),
                "where it says USER and PASSWORD: \
                 web.env.API_URL=postgres://USER:PASSWORD@${{ db.PLOYZ_PRIVATE_DOMAIN }}:5432/shop",
            ),
            (
                format!("http://{DB}/?token=EXAMPLE"),
                "where it says QUERY: \
                 web.env.API_URL=http://${{ db.PLOYZ_PRIVATE_DOMAIN }}/?QUERY",
            ),
        ] {
            let said = set_instead(
                &root,
                &path("web.env.API_URL"),
                &Reference::of_value(&value),
                &scope,
            )
            .unwrap_err();
            assert!(said.ends_with(by_hand), "{said}");
            assert!(!said.contains("EXAMPLE"), "{said}");
        }
    }
}
