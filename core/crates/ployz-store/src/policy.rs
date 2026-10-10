//! Deployment Policy: whether a push to a Git Service's branch deploys it
//! (`autoDeploy`), whether that waits for the commit's CI to pass (`waitForCi`), and
//! which changed paths count (`watchPaths`). Each Service identity owns its policy,
//! kept outside Working State, so a change takes effect at once and no Deploy ships it.

use ployz_core::RpcError;
use ployz_core::config::{AuthoredServiceConfig, ServiceSource};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::Actor;
use crate::id::EnvironmentId;
use crate::settings::ServiceSetting;
use crate::storage::Tx;

/// The most watch paths a Service keeps.
const WATCH_PATHS: usize = 50;

/// One Service's Deployment Policy, as stored.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Policy {
    pub(crate) auto_deploy: bool,
    pub(crate) wait_for_ci: bool,
    pub(crate) watch_paths: Vec<String>,
    /// Who builds it first: `github`, or a Server's Machine ID. None follows the
    /// Organization's Build Order; see [`crate::builders`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) preferred_builder: Option<String>,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            auto_deploy: true,
            wait_for_ci: false,
            watch_paths: Vec::new(),
            preferred_builder: None,
        }
    }
}

impl Policy {
    /// Whether a push that changed `changed` deploys this Service: any changed path
    /// the watch paths select, the last matching pattern winning. Without watch
    /// paths any change counts.
    pub(crate) fn watches(&self, changed: &[String]) -> bool {
        if self.watch_paths.is_empty() {
            return !changed.is_empty();
        }
        let patterns: Vec<Pattern> = self
            .watch_paths
            .iter()
            .filter_map(|pattern| Pattern::compile(pattern))
            .collect();
        changed.iter().any(|path| {
            patterns.iter().fold(false, |selected, pattern| {
                if pattern.matches(path) {
                    !pattern.negated
                } else {
                    selected
                }
            })
        })
    }
}

/// The Settings of a Git Service's Deployment Policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PolicySetting {
    AutoDeploy,
    WaitForCi,
    WatchPaths,
    PreferredBuilder,
}

impl PolicySetting {
    pub(crate) const ALL: [Self; 4] = [
        Self::AutoDeploy,
        Self::WaitForCi,
        Self::WatchPaths,
        Self::PreferredBuilder,
    ];

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::AutoDeploy => "autoDeploy",
            Self::WaitForCi => "waitForCi",
            Self::WatchPaths => "watchPaths",
            Self::PreferredBuilder => "preferredBuilder",
        }
    }

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::AutoDeploy => "Auto-deploy",
            Self::WaitForCi => "Wait for CI",
            Self::WatchPaths => "Watch paths",
            Self::PreferredBuilder => "Preferred Builder",
        }
    }

    pub(crate) const fn description(self) -> &'static str {
        match self {
            Self::AutoDeploy => {
                "Deploy the Environment's Saved State when its branch gets a new commit. Takes effect at once."
            }
            Self::WaitForCi => {
                "Hold an auto-deploy until every GitHub check suite of the commit passes. Takes effect at once."
            }
            Self::WatchPaths => {
                "Gitignore-style patterns of repository paths a push must change to auto-deploy; the last matching pattern wins and ! excludes. Empty means any change. Takes effect at once."
            }
            Self::PreferredBuilder => {
                "Who builds this Service first: \"github\" for GitHub Actions, or a Server's Machine ID; then the Organization's Build Order, without it. Unset follows the Build Order. Applies to the next build."
            }
        }
    }

    pub(crate) fn default(self) -> Value {
        match self {
            Self::AutoDeploy => json!(true),
            Self::WaitForCi => json!(false),
            Self::WatchPaths => json!([]),
            Self::PreferredBuilder => Value::Null,
        }
    }

    pub(crate) fn expected(self) -> Value {
        match self {
            Self::AutoDeploy | Self::WaitForCi => json!({ "type": "boolean" }),
            Self::WatchPaths => json!({
                "type": "array",
                "items": { "type": "string", "minLength": 1, "maxLength": 255 },
                "maxItems": WATCH_PATHS,
            }),
            Self::PreferredBuilder => json!({
                "type": "string",
                "anyOf": [{ "const": "github" }, { "pattern": "^[0-9a-f]{32}$" }],
            }),
        }
    }

    pub(crate) fn examples(self) -> Value {
        match self {
            Self::AutoDeploy => json!([false]),
            Self::WaitForCi => json!([true]),
            Self::WatchPaths => json!([["apps/web/**", "!**/*.md"]]),
            Self::PreferredBuilder => json!(["github"]),
        }
    }

    /// Only a Git Service deploys on push.
    pub(crate) const fn applies(config: &AuthoredServiceConfig) -> bool {
        matches!(config.source, ServiceSource::Git { .. })
    }

    pub(crate) fn value(self, policy: &Policy) -> Value {
        match self {
            Self::AutoDeploy => json!(policy.auto_deploy),
            Self::WaitForCi => json!(policy.wait_for_ci),
            Self::WatchPaths => json!(policy.watch_paths),
            Self::PreferredBuilder => json!(policy.preferred_builder),
        }
    }

    /// Write `value` into `policy`, or the default when none. Text is accepted, as
    /// `set PATH=VALUE` sends it: `true`/`false`, or one watch path.
    fn write(self, policy: &mut Policy, value: Option<Value>) -> Result<(), RpcError> {
        let setting = ServiceSetting::Policy(self);
        if self == Self::PreferredBuilder {
            policy.preferred_builder = value
                .map(|value| crate::builders::Preferred::parse(&value).map(|p| p.text()))
                .transpose()?;
            return Ok(());
        }
        let value = value.unwrap_or_else(|| self.default());
        match self {
            Self::AutoDeploy | Self::WaitForCi => {
                let flag = match &value {
                    Value::Bool(flag) => *flag,
                    Value::String(text) if text.trim() == "true" => true,
                    Value::String(text) if text.trim() == "false" => false,
                    Value::Null
                    | Value::Number(_)
                    | Value::String(_)
                    | Value::Array(_)
                    | Value::Object(_) => {
                        return Err(setting.invalid("expected true or false"));
                    }
                };
                if self == Self::AutoDeploy {
                    policy.auto_deploy = flag;
                } else {
                    policy.wait_for_ci = flag;
                }
            }
            Self::WatchPaths => {
                let paths = match value {
                    Value::String(text) => vec![text],
                    value @ (Value::Null
                    | Value::Bool(_)
                    | Value::Number(_)
                    | Value::Array(_)
                    | Value::Object(_)) => setting.decode::<Vec<String>>(value)?,
                };
                let paths: Vec<String> = paths.iter().map(|path| path.trim().to_owned()).collect();
                if paths.len() > WATCH_PATHS
                    || paths
                        .iter()
                        .any(|path| path.len() > 255 || Pattern::compile(path).is_none())
                {
                    return Err(setting.invalid(
                        "expected up to 50 repository path patterns, like src/** or !docs/",
                    ));
                }
                policy.watch_paths = paths;
            }
            // Written above.
            Self::PreferredBuilder => {}
        }
        Ok(())
    }
}

/// `service`'s Deployment Policy in `environment`; the default until one is set.
pub(crate) fn load(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    service: &str,
) -> Result<Policy, RpcError> {
    Ok(stored(tx, environment, service)?.unwrap_or_default())
}

/// `service`'s Deployment Policy in `environment`, if one was ever set.
pub(crate) fn stored(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    service: &str,
) -> Result<Option<Policy>, RpcError> {
    tx.query(
        "SELECT policy FROM config_service_policy WHERE environment_id = ?1 AND service_id = ?2",
        &[environment.as_str().into(), service.into()],
    )?
    .first()
    .map(|row| row.json(0, "Deployment Policy"))
    .transpose()
}

/// Store `policy` as `service`'s Deployment Policy in `environment`.
pub(crate) fn store(
    tx: &mut dyn Tx,
    who: &Actor,
    environment: &EnvironmentId,
    service: &str,
    policy: &Policy,
) -> Result<(), RpcError> {
    tx.execute(
        "INSERT INTO config_service_policy (environment_id, service_id, organization_id, policy) \
         VALUES (?1, ?2, ?3, ?4) \
         ON CONFLICT (environment_id, service_id) DO UPDATE SET policy = excluded.policy",
        &[
            environment.as_str().into(),
            service.into(),
            who.organization.as_str().into(),
            serde_json::to_string(policy)
                .expect("a policy is JSON")
                .as_str()
                .into(),
        ],
    )?;
    Ok(())
}

/// Set (or with `None`, unset) one Setting of Git Service `service`'s policy.
/// Returns whether it changed.
pub(crate) fn set(
    tx: &mut dyn Tx,
    who: &Actor,
    environment: &EnvironmentId,
    service: &ployz_core::config::SavedServiceIntent,
    setting: PolicySetting,
    value: Option<Value>,
) -> Result<bool, RpcError> {
    if !PolicySetting::applies(&service.config) {
        return Err(ServiceSetting::Policy(setting)
            .invalid("this Setting requires a Service connected to a repository"));
    }
    let before = load(tx, environment, &service.id)?;
    let mut policy = before.clone();
    setting.write(&mut policy, value)?;
    if policy == before {
        return Ok(false);
    }
    store(tx, who, environment, &service.id, &policy)?;
    Ok(true)
}

/// Whether `path` is a canonical repository-relative path, as GitHub reports changes.
pub(crate) fn is_repository_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains(['\\', '\0'])
        && !is_drive(path)
        && path.split('/').all(|part| !matches!(part, "" | "." | ".."))
}

fn is_drive(path: &str) -> bool {
    matches!(path.as_bytes(), [letter, b':', b'/', ..] if letter.is_ascii_alphabetic())
}

/// One watch path: a gitignore-style pattern. Without a slash it matches a file name
/// anywhere; a leading `/` anchors it at the root; a trailing `/` selects everything
/// under a directory. `*`, `?`, `**`, `[…]` and `{a,b}` glob; dotfiles match.
struct Pattern {
    negated: bool,
    /// Match the path's last segment only.
    base: bool,
    regex: Regex,
}

impl Pattern {
    fn compile(pattern: &str) -> Option<Self> {
        let (negated, rest) = match pattern.strip_prefix('!') {
            Some(rest) => (true, rest),
            None => (false, pattern),
        };
        if rest.is_empty() || rest.starts_with(['!']) || rest.starts_with("//") {
            return None;
        }
        let anchored = rest.starts_with('/');
        let rest = rest.strip_prefix('/').unwrap_or(rest);
        let directory = rest.ends_with('/');
        let rest = rest.strip_suffix('/').unwrap_or(rest);
        if rest.is_empty() || rest.contains(['\\', '\0']) || !is_repository_path(rest) {
            return None;
        }
        let slash = rest.contains('/');
        let glob = match (directory, anchored || slash) {
            (true, true) => format!("{rest}/**"),
            (true, false) => format!("**/{rest}/**"),
            (false, _) => rest.to_owned(),
        };
        let mut regex = String::from("^");
        let segments: Vec<&str> = glob.split('/').collect();
        for (index, segment) in segments.iter().enumerate() {
            let last = index + 1 == segments.len();
            match (*segment, last) {
                ("**", true) => regex.push_str(".*"),
                ("**", false) => regex.push_str("(?:[^/]+/)*"),
                (segment, _) => {
                    regex.push_str(&glob_segment(segment)?);
                    if !last {
                        regex.push('/');
                    }
                }
            }
        }
        regex.push('$');
        Some(Self {
            negated,
            base: !anchored && !directory && !slash,
            regex: Regex::new(&regex).ok()?,
        })
    }

    fn matches(&self, path: &str) -> bool {
        let path = if self.base {
            path.rsplit('/').next().unwrap_or(path)
        } else {
            path
        };
        self.regex.is_match(path)
    }
}

/// One path segment's glob as a regex; `None` when it is malformed.
fn glob_segment(glob: &str) -> Option<String> {
    let chars: Vec<char> = glob.chars().collect();
    let mut regex = String::new();
    let mut index = 0;
    while let Some(&char) = chars.get(index) {
        let next = chars.get(index + 1).copied();
        match char {
            // Extglobs are not supported.
            '?' | '*' | '+' | '@' | '!' if next == Some('(') => return None,
            '*' => {
                while chars.get(index + 1) == Some(&'*') {
                    index += 1;
                }
                regex.push_str("[^/]*");
            }
            '?' => regex.push_str("[^/]"),
            '[' => {
                let start = index + 1;
                let mut end = start;
                if matches!(chars.get(end), Some('!' | '^')) {
                    end += 1;
                }
                if chars.get(end) == Some(&']') {
                    end += 1;
                }
                while chars.get(end).is_some_and(|&char| char != ']') {
                    end += 1;
                }
                chars.get(end)?;
                regex.push('[');
                for (offset, &char) in chars.get(start..end)?.iter().enumerate() {
                    match char {
                        '!' | '^' if offset == 0 => regex.push('^'),
                        '-' => regex.push('-'),
                        char => regex.push_str(&regex::escape(&char.to_string())),
                    }
                }
                regex.push(']');
                index = end;
            }
            '{' => {
                let mut depth = 0;
                let mut end = index;
                let mut alternatives = Vec::new();
                let mut from = index + 1;
                loop {
                    match chars.get(end)? {
                        '{' => depth += 1,
                        '}' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        ',' if depth == 1 => {
                            alternatives.push(chars.get(from..end)?.iter().collect::<String>());
                            from = end + 1;
                        }
                        _ => {}
                    }
                    end += 1;
                }
                alternatives.push(chars.get(from..end)?.iter().collect::<String>());
                let alternatives = alternatives
                    .iter()
                    .map(|alternative| glob_segment(alternative))
                    .collect::<Option<Vec<_>>>()?;
                regex.push_str(&format!("(?:{})", alternatives.join("|")));
                index = end;
            }
            '}' | ']' => return None,
            char => regex.push_str(&regex::escape(&char.to_string())),
        }
        index += 1;
    }
    Some(regex)
}

#[cfg(test)]
mod tests {
    use super::Policy;

    fn watches(paths: &[&str], changed: &[&str]) -> bool {
        Policy {
            watch_paths: paths.iter().map(|path| (*path).to_owned()).collect(),
            ..Policy::default()
        }
        .watches(
            &changed
                .iter()
                .map(|path| (*path).to_owned())
                .collect::<Vec<_>>(),
        )
    }

    #[test]
    fn watch_paths_match_as_the_dashboard_did() {
        assert!(watches(
            &["src/**", "!src/docs/**", "src/docs/ship.md"],
            &[
                ".github/workflows/ci.yml",
                "nested/README.md",
                "src/docs/guide.md",
                "src/docs/ship.md"
            ],
        ));
        assert!(watches(&[], &["src/main.ts"]));
        assert!(!watches(&[], &[]));
        assert!(watches(&["README.md"], &["nested/README.md"]));
        assert!(!watches(&["/README.md"], &["nested/README.md"]));
        assert!(watches(&["docs/"], &["nested/docs/guide.md"]));
        assert!(watches(
            &["packages/**/src/**"],
            &["packages/api/src/index.ts"]
        ));
        assert!(watches(&["**/.github/**"], &[".github/workflows/ci.yml"]));
        assert!(!watches(&["src/**"], &["docs/guide.md"]));
        assert!(!watches(
            &["src/**", "!src/docs/**"],
            &["src/docs/guide.md"]
        ));
        assert!(watches(&["*.{ts,tsx}"], &["app/page.tsx"]));
        assert!(watches(&["src/[ab]*.rs"], &["src/b1.rs"]));
        assert!(!watches(&["src/[!ab]*.rs"], &["src/b1.rs"]));
    }

    #[test]
    fn malformed_watch_paths_are_refused() {
        for pattern in [
            "",
            "!",
            "!!src/**",
            "../src/**",
            "src/../secret",
            "src\\**",
            "src/[abc",
            "//src/**",
            "C:/src/**",
            "src/{a,b",
            "+(a)",
        ] {
            assert!(super::Pattern::compile(pattern).is_none(), "{pattern}");
        }
    }
}
