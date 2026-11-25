//! What output may repeat of a value typed. A hint, an error or `--json` repeats one only
//! as a [`Shown`], whose constructors keep the parts whose kind is never a secret and put a
//! named placeholder in place of everything else. Nothing typed is recognized as a secret:
//! what isn't known to be safe is withheld.

use std::fmt;

use ployz_core::ServiceName;
use ployz_core::config::{ValuePart, parse_variable_template};
use serde::Serialize;

/// A value typed, as output may repeat it.
#[derive(Clone, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct Shown(String);

impl Shown {
    /// A placeholder for a value withheld whole, named for what goes there: `VALUE`.
    pub(crate) fn placeholder(name: &'static str) -> Self {
        Self(name.to_owned())
    }

    /// The placeholder `arg` declares: its value name, else its id, `--cloud-url` as
    /// `CLOUD_URL`.
    pub(crate) fn declared(arg: &clap::Arg) -> Self {
        Self(arg.get_value_names().and_then(<[_]>::first).map_or_else(
            || arg.get_id().as_str().to_ascii_uppercase().replace('-', "_"),
            ToString::to_string,
        ))
    }

    /// `typed` whole, for a value whose kind is never a secret, such as a name, a number,
    /// a choice or a path. `clippy.toml` refuses it except where the kind is known.
    pub(crate) fn verbatim(typed: &str) -> Self {
        Self(typed.to_owned())
    }

    /// A value of Ployz references alone, such as `${{ db.PLOYZ_PRIVATE_DOMAIN }}`; none
    /// when any text was typed around them or a reference names no Service.
    pub(crate) fn references(typed: &str) -> Option<Self> {
        let parsed = parse_variable_template(typed, |name, _| {
            ServiceName::parse(name).ok().map(|_| name.to_owned())
        });
        let only = !parsed.parts.is_empty()
            && parsed
                .parts
                .iter()
                .all(|part| matches!(part, ValuePart::Ref { .. }));
        only.then(|| Self(typed.to_owned()))
    }

    /// A URL or a bare host: its scheme when it is one of `schemes`, else `SCHEME`; its
    /// host and port when each has that shape, else `HOST` and `PORT`; its user info as
    /// `USER` or `USER:PASSWORD`; never its path, query or fragment.
    pub(crate) fn url(typed: &str, schemes: &[&str]) -> Self {
        let (scheme, rest) = match typed.split_once("://") {
            Some((scheme, rest)) => (Some(scheme), rest),
            None => (None, typed),
        };
        let (authority, after) = rest.split_at(rest.find(['/', '?', '#']).unwrap_or(rest.len()));
        let (user_info, host) = match authority.rsplit_once('@') {
            Some((user_info, host)) => (Some(user_info), host),
            None => (None, authority),
        };
        let mut shown = String::new();
        if let Some(scheme) = scheme {
            shown.push_str(if schemes.contains(&scheme) {
                scheme
            } else {
                "SCHEME"
            });
            shown.push_str("://");
        }
        shown.push_str(match user_info {
            Some(user_info) if user_info.contains(':') => "USER:PASSWORD@",
            Some(_) => "USER@",
            None => "",
        });
        // An `@` past the authority may close user info that holds a `/`, `?` or `#`, so
        // what reads as the host may be part of a password.
        if after.contains('@') {
            shown.push_str("HOST");
            return Self(shown);
        }
        let (host, port) = split_port(host);
        shown.push_str(if is_host(host) { host } else { "HOST" });
        if let Some(port) = port {
            shown.push(':');
            shown.push_str(if is_port(port) { port } else { "PORT" });
        }
        Self(shown)
    }

    /// `name=value`.
    pub(crate) fn assignment(name: Self, value: Self) -> Self {
        Self(format!("{}={}", name.0, value.0))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Shown {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.0, formatter)
    }
}

impl fmt::Display for Shown {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// `host` and the port after its last `:`; a bracketed IPv6 address keeps its own.
fn split_port(text: &str) -> (&str, Option<&str>) {
    let close = text.starts_with('[').then(|| text.find(']')).flatten();
    let from = close.unwrap_or(0);
    match text.get(from..).and_then(|rest| rest.rfind(':')) {
        Some(colon) => (
            text.get(..from + colon).unwrap_or_default(),
            text.get(from + colon + 1..),
        ),
        None => (text, None),
    }
}

fn is_host(host: &str) -> bool {
    let bracketed = host
        .strip_prefix('[')
        .and_then(|inner| inner.strip_suffix(']'))
        .is_some_and(|inner| {
            !inner.is_empty()
                && inner
                    .chars()
                    .all(|c| c.is_ascii_hexdigit() || c == ':' || c == '.')
        });
    bracketed
        || (!host.is_empty()
            && host.len() <= 253
            && host
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-'))
}

fn is_port(port: &str) -> bool {
    !port.is_empty() && port.len() <= 5 && port.bytes().all(|b| b.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::Shown;

    /// A URL keeps its scheme, host and port alone, wherever a secret hides in the rest.
    #[test]
    fn a_url_shows_its_scheme_host_and_port_and_nothing_else_typed() {
        let schemes = &["https", "ssh"];
        for (typed, shown) in [
            ("https://cloud.example:8443", "https://cloud.example:8443"),
            ("cloud.example", "cloud.example"),
            ("[::1]:22", "[::1]:22"),
            // A password that holds `://`.
            ("ssh://root:x://EXAMPLE@203.0.113.7", "ssh://HOST"),
            (
                "ssh://root:EXAMPLE@203.0.113.7:22",
                "ssh://USER:PASSWORD@203.0.113.7:22",
            ),
            // A token in the path, the query or the fragment.
            (
                "https://cloud.example/EXAMPLE?t=EXAMPLE#EXAMPLE",
                "https://cloud.example",
            ),
            // A token as the user.
            (
                "https://EXAMPLE@cloud.example",
                "https://USER@cloud.example",
            ),
            // A URL in the query, with a user of its own.
            (
                "https://cloud.example/?next=https://EXAMPLE@x.example/",
                "https://HOST",
            ),
            // A password that holds a `/`, so its `@` sits past the authority.
            ("root:EXAMPLE/EXAMPLE@203.0.113.7", "HOST"),
            // A scheme of its own, a host and a port that aren't one.
            ("EXAMPLE://cloud.example", "SCHEME://cloud.example"),
            ("ssh://EXAMPLE EXAMPLE", "ssh://HOST"),
            ("ssh://203.0.113.7:EXAMPLE", "ssh://203.0.113.7:PORT"),
        ] {
            assert_eq!(Shown::url(typed, schemes).as_str(), shown, "{typed}");
        }
    }

    /// References alone show; text typed beside them, or in place of a Service's name,
    /// makes the value one to withhold.
    #[test]
    fn a_value_shows_only_when_it_is_references_alone() {
        for (typed, shown) in [
            ("${{ db.PLOYZ_PRIVATE_DOMAIN }}", true),
            ("${{ db.PLOYZ_PRIVATE_DOMAIN }}${{ PORT }}", true),
            ("${{ db.PLOYZ_PRIVATE_DOMAIN }}:5432", false),
            ("EXAMPLE=${{ db.PLOYZ_PRIVATE_DOMAIN }}", false),
            ("${{ EXAMPLE_token.KEY }}", false),
            ("EXAMPLE", false),
            ("", false),
        ] {
            assert_eq!(Shown::references(typed).is_some(), shown, "{typed}");
        }
    }
}
