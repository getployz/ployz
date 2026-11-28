//! What output may repeat of a value typed. A hint, an error or `--json` repeats one only
//! as a [`Shown`], built from a value already parsed into a [`Public`] type, or as a named
//! placeholder. Text is never shown because of the argument it was typed for: it shows only
//! once it parsed as that argument's kind, and the parse refused every other shape.

use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};

use ployz_core::config::{ValuePart, parse_variable_template};
use ployz_core::{
    ClusterName, ConfigFileName, ConfigName, DomainPrefix, IngressHost, MachineId, MachineLabelKey,
    MachineRelease, Namespace, ServiceName, StorageChoice, StoragePreparation, TrashId,
};
use ployz_store::{
    BranchName, EnvironmentName, PlainValue, ProjectName, RegionName, RepositoryName,
    ReviewVersion, SettingPath, SyncId, VolumeName,
};
use serde::Serialize;
use url::Url;

/// A value typed, as output may repeat it.
#[derive(Clone, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct Shown(String);

/// A parsed type whose every value output may repeat: its parse refuses any text but a
/// number, a choice, a name, an ID Ployz printed, or a path or URL with nothing in it but
/// where it points. It is sealed: the list at the end of this file is every such type.
pub(crate) trait Public: fmt::Display + sealed::Sealed {}

mod sealed {
    pub trait Sealed {}
}

impl Shown {
    /// A value withheld whole, named for what goes there: `<VALUE>`. The command line
    /// refuses a value that holds one ([`placeholder_in`]), so a line that shows one never
    /// runs as printed.
    pub(crate) fn placeholder(name: &'static str) -> Self {
        Self(format!("<{name}>"))
    }

    /// A fixed word compiled into Ployz, such as `tcp` or `none`.
    pub(crate) fn word(word: &'static str) -> Self {
        Self(word.to_owned())
    }

    /// The placeholder `arg` declares: its value name, else its id, `--cloud-url` as
    /// `<CLOUD_URL>`.
    pub(crate) fn declared(arg: &clap::Arg) -> Self {
        let name = arg.get_value_names().and_then(<[_]>::first).map_or_else(
            || arg.get_id().as_str().replace('-', "_"),
            ToString::to_string,
        );
        Self(format!("<{}>", name.to_ascii_uppercase()))
    }

    /// `value`, which parsed into a type output may repeat.
    pub(crate) fn of(value: &impl Public) -> Self {
        Self(value.to_string())
    }

    /// `left`, `between` and `right`, as `SERVICE=IMAGE` or `OWNER/REPO@BRANCH` joins them.
    pub(crate) fn joined(left: Self, between: &'static str, right: Self) -> Self {
        Self(format!("{}{between}{}", left.0, right.0))
    }

    /// `url` whole, when it is a scheme of `schemes`, a host and a port alone; none when it
    /// has user info, a path, a query or a fragment, which output never repeats, so a
    /// command that showed the rest would reach somewhere else.
    pub(crate) fn url(url: &Url, schemes: &[&str]) -> Option<Self> {
        let shown = Self::url_redacted(url, schemes);
        (shown.0 == origin(url)).then_some(shown)
    }

    /// `url` as an error names it: its scheme, host and port, with each other part it has
    /// named in its place, as in
    /// `https://<USER>:<PASSWORD>@cloud.example/<PATH>?<QUERY>#<FRAGMENT>`. A scheme not in
    /// `schemes` shows as `<SCHEME>`. When an `@` follows the authority, user info may have
    /// held a `/`, so what parsed as the host and port shows as `<HOST>`.
    pub(crate) fn url_redacted(url: &Url, schemes: &[&str]) -> Self {
        let scheme = url.scheme();
        let mut shown = match schemes.contains(&scheme) {
            true => format!("{scheme}://"),
            false => "<SCHEME>://".to_owned(),
        };
        match (url.username().is_empty(), url.password().is_some()) {
            (_, true) => shown.push_str("<USER>:<PASSWORD>@"),
            (false, false) => shown.push_str("<USER>@"),
            (true, false) => {}
        }
        match url[url::Position::BeforePath..].contains('@') {
            true => shown.push_str("<HOST>"),
            false => shown.push_str(&host_and_port(url)),
        }
        if !matches!(url.path(), "" | "/") {
            shown.push_str("/<PATH>");
        }
        if url.query().is_some() {
            shown.push_str("?<QUERY>");
        }
        if url.fragment().is_some() {
            shown.push_str("#<FRAGMENT>");
        }
        Self(shown)
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

/// Whether `typed` holds a placeholder output printed in place of a value, such as `<VALUE>`
/// in `web.env.KEY=<VALUE>`: `<`, an uppercase letter, then uppercase letters, digits or
/// `_/[]@=.:-`, then `>`.
pub(crate) fn placeholder_in(typed: &str) -> bool {
    typed.split('<').skip(1).any(|after| {
        after.split_once('>').is_some_and(|(name, _)| {
            name.starts_with(|c: char| c.is_ascii_uppercase())
                && name.chars().all(|c| {
                    c.is_ascii_uppercase() || c.is_ascii_digit() || "_/[]@=.:-".contains(c)
                })
        })
    })
}

/// Whether `typed` may hold URL user info: it holds `://`, or an `@` before its first `/`.
/// Such text never shows as a path, whatever it ends with.
pub(crate) fn may_hold_user_info(typed: &str) -> bool {
    typed.contains("://")
        || typed
            .split('/')
            .next()
            .is_some_and(|head| head.contains('@'))
}

/// Whether `typed` is one placeholder whole, `<COMMAND>`.
pub(crate) fn is_placeholder(typed: &str) -> bool {
    typed
        .strip_prefix('<')
        .and_then(|rest| rest.strip_suffix('>'))
        .is_some_and(|name| !name.contains(['<', '>']))
        && placeholder_in(typed)
}

/// `scheme://host[:port]`, the parts of `url` a person reads as where it points.
fn origin(url: &Url) -> String {
    format!("{}://{}", url.scheme(), host_and_port(url))
}

fn host_and_port(url: &Url) -> String {
    let host = url.host_str().unwrap_or("HOST");
    match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_owned(),
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

/// One lowercase DNS label: the rule Ployz names Servers, Contexts, Organizations,
/// Containers and Branch nodes by, where no narrower type parses the argument.
#[derive(Clone, Debug)]
pub(crate) struct Label(ServiceName);

impl Label {
    pub(crate) fn parse(text: &str) -> Option<Self> {
        ServiceName::parse(text).ok().map(Self)
    }
}

impl fmt::Display for Label {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// An ID or digest Cloud or a Store minted and printed, such as an approval digest or a
/// Deployment's `#12`: ASCII letters, digits and `-_.:#`, at most 128.
#[derive(Clone, Debug)]
pub(crate) struct Minted(String);

impl Minted {
    pub(crate) fn parse(text: &str) -> Option<Self> {
        let minted = (1..=128).contains(&text.len())
            && text
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_.:#".contains(&byte));
        minted.then(|| Self(text.to_owned()))
    }
}

impl fmt::Display for Minted {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// An image reference Docker can pull, such as `nginx:1.27`: the rule the Store checks.
#[derive(Clone, Debug)]
pub(crate) struct ImageReference(String);

impl ImageReference {
    pub(crate) fn parse(text: &str) -> Option<Self> {
        ployz_core::is_image_reference(text).then(|| Self(text.to_owned()))
    }
}

impl fmt::Display for ImageReference {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// A path that names a file or directory on this machine, as a command reads one, or `-`
/// for stdin. Ployz names the files it reads in its own errors.
#[derive(Clone, Debug)]
pub(crate) struct ExistingPath(PathBuf);

impl ExistingPath {
    pub(crate) fn parse(text: &str) -> Option<Self> {
        let path = Path::new(text);
        let exists = text == "-" || path.try_exists().is_ok_and(|exists| exists);
        (exists && !may_hold_user_info(text)).then(|| Self(path.to_owned()))
    }
}

impl fmt::Display for ExistingPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.display().fmt(formatter)
    }
}

/// A Server name, ID or glob over names, such as `web-*`: lowercase letters, digits, `-`,
/// `.`, `*` and `?`, at most 253.
#[derive(Clone, Debug)]
pub(crate) struct NameGlob(String);

impl NameGlob {
    pub(crate) fn parse(text: &str) -> Option<Self> {
        let named = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || "-.*?".contains(c);
        (!text.is_empty() && text.len() <= 253 && text.chars().all(named))
            .then(|| Self(text.to_owned()))
    }
}

impl fmt::Display for NameGlob {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// A Ployz config path: a YAML file by its name, or one this computer keeps Ployz state
/// at, the config or a file Ployz writes beside it such as the Cloud sign-in.
#[derive(Clone, Debug)]
pub(crate) struct ConfigPath(PathBuf);

impl ConfigPath {
    pub(crate) fn parse(text: &str) -> Option<Self> {
        let path = Path::new(text);
        let yaml = path
            .extension()
            .is_some_and(|extension| extension == "yaml" || extension == "yml");
        let named = yaml || crate::config_dir::ConfigDir::new(path).kept();
        (named && !may_hold_user_info(text) && !Self::reads_as_url(text))
            .then(|| Self(path.to_owned()))
    }

    /// Whether `text` is a URL, `https://…` or `user:…@host/…`, parsed or not, which
    /// `--ployz-config` refuses: it names no file, and its user info may be a credential
    /// that every error naming the config would repeat. A one-letter scheme is a Windows
    /// drive.
    pub(crate) fn reads_as_url(text: &str) -> bool {
        text.contains("://") || Url::parse(text).is_ok_and(|url| url.scheme().len() > 1)
    }
}

impl fmt::Display for ConfigPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.display().fmt(formatter)
    }
}

/// A value of Ployz references alone, such as `${{ db.PLOYZ_PRIVATE_DOMAIN }}`.
#[derive(Clone, Debug)]
pub(crate) struct References(String);

impl References {
    /// None when any text was typed around the references or one names no Service.
    pub(crate) fn parse(typed: &str) -> Option<Self> {
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
}

impl fmt::Display for References {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// A choice the command tree declares, compiled or published by Cloud.
pub(crate) struct Choice<'a>(pub(crate) &'a clap::builder::PossibleValue);

impl fmt::Display for Choice<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0.get_name())
    }
}

/// A `--patch` whose every value validated as its Setting's type, as JSON again.
pub(crate) struct PlainPatch(pub(crate) serde_json::Map<String, serde_json::Value>);

impl fmt::Display for PlainPatch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        serde_json::Value::Object(self.0.clone()).fmt(formatter)
    }
}

macro_rules! public {
    ($($kind:ty),+ $(,)?) => {
        $(
            impl sealed::Sealed for $kind {}
            impl Public for $kind {}
        )+
    };
}

public!(
    Choice<'_>,
    Label,
    Minted,
    ImageReference,
    ExistingPath,
    ConfigPath,
    NameGlob,
    References,
    u16,
    u32,
    u64,
    usize,
    i64,
    IpAddr,
    SocketAddr,
    ipnet::Ipv4Net,
    ServiceName,
    ClusterName,
    ConfigName,
    ConfigFileName,
    DomainPrefix,
    IngressHost,
    MachineId,
    MachineLabelKey,
    Namespace,
    MachineRelease,
    StorageChoice,
    StoragePreparation,
    BranchName,
    EnvironmentName,
    ProjectName,
    RegionName,
    RepositoryName,
    ReviewVersion,
    SyncId,
    TrashId,
    VolumeName,
    SettingPath,
    PlainValue,
    PlainPatch,
    ployz_core::first_key::LineKey,
);

#[cfg(test)]
mod tests {
    use super::{References, Shown};

    /// A URL shows whole only when it is where it points and nothing else; otherwise an
    /// error names each part withheld, and a command has none to run.
    #[test]
    fn a_url_shows_whole_only_with_nothing_in_it_but_where_it_points() {
        for (typed, whole, redacted) in [
            (
                "https://cloud.example:8443",
                Some("https://cloud.example:8443"),
                "https://cloud.example:8443",
            ),
            (
                "https://cloud.example/",
                Some("https://cloud.example"),
                "https://cloud.example",
            ),
            (
                "http://[::1]:3000",
                Some("http://[::1]:3000"),
                "http://[::1]:3000",
            ),
            // A path a Cloud is served under: where the retry would point is elsewhere.
            (
                "https://example.test/ployz",
                None,
                "https://example.test/<PATH>",
            ),
            (
                "https://EXAMPLE:EXAMPLE@cloud.example:8443/EXAMPLE?t=EXAMPLE#EXAMPLE",
                None,
                "https://<USER>:<PASSWORD>@cloud.example:8443/<PATH>?<QUERY>#<FRAGMENT>",
            ),
            (
                "https://EXAMPLE@cloud.example",
                None,
                "https://<USER>@cloud.example",
            ),
            (
                "https://cloud.example/?next=https://EXAMPLE@x.example/",
                None,
                "https://<HOST>?<QUERY>",
            ),
            // A password with a `/` and digits before it, which parse as a port.
            (
                "https://EXAMPLE:1234/EXAMPLE@cloud.example",
                None,
                "https://<HOST>/<PATH>",
            ),
            ("ftp://cloud.example", None, "<SCHEME>://cloud.example"),
        ] {
            let url = url::Url::parse(typed).unwrap();
            let schemes = &["http", "https"];
            let shown = Shown::url(&url, schemes);
            assert_eq!(shown.as_ref().map(Shown::as_str), whole, "{typed}");
            assert_eq!(
                Shown::url_redacted(&url, schemes).as_str(),
                redacted,
                "{typed}"
            );
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
            assert_eq!(References::parse(typed).is_some(), shown, "{typed}");
        }
    }
}
