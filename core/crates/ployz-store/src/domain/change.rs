//! Staged domain changes read off the review rows that store them, so no surface
//! names the fields domains are stored in.

use std::collections::BTreeMap;
use std::fmt;

use ployz_core::config::{ServiceManagedHostname, ServiceRoute, ServiceSettingChange};
use serde::Deserialize;
use serde_json::Value;

/// What happens to one domain.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DomainEdit {
    /// The container port it reaches; `None` follows the container's `PORT`.
    Added {
        port: Option<u16>,
    },
    Removed,
    Repointed {
        to: Option<u16>,
    },
}

/// One domain a staged change adds, removes or repoints.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DomainChange {
    /// Its hostname, or a generated domain's prefix.
    pub domain: String,
    pub edit: DomainEdit,
}

impl DomainChange {
    /// The domain changes a review row stages, or `None` when it stores no domain.
    #[must_use]
    pub fn of(row: &ServiceSettingChange) -> Option<Vec<Self>> {
        let (_, field) = row.path.split_once('.')?;
        let read: fn(&Value) -> BTreeMap<String, Option<u16>> = if field == "managedHostnames" {
            |value| {
                Option::<Vec<ServiceManagedHostname>>::deserialize(value)
                    .ok()
                    .flatten()
                    .into_iter()
                    .flatten()
                    .map(|one| (one.prefix, one.target_port))
                    .collect()
            }
        } else if field.starts_with("routes.") {
            |value| {
                Option::<ServiceRoute>::deserialize(value)
                    .ok()
                    .flatten()
                    .map(|one| (one.hostname, one.target_port))
                    .into_iter()
                    .collect()
            }
        } else {
            return None;
        };
        let before = read(&row.before);
        let after = read(&row.after);
        let removed = before
            .keys()
            .filter(|domain| !after.contains_key(*domain))
            .map(|domain| (domain, DomainEdit::Removed));
        let changed = after
            .iter()
            .filter_map(|(domain, &port)| match before.get(domain) {
                None => Some((domain, DomainEdit::Added { port })),
                Some(&from) if from != port => Some((domain, DomainEdit::Repointed { to: port })),
                Some(_) => None,
            });
        Some(
            removed
                .chain(changed)
                .map(|(domain, edit)| Self {
                    domain: domain.clone(),
                    edit,
                })
                .collect(),
        )
    }
}

impl fmt::Display for DomainChange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let domain = &self.domain;
        match self.edit {
            DomainEdit::Added { port: None } => write!(f, "added {domain}"),
            DomainEdit::Added { port: Some(port) } => write!(f, "added {domain} on port {port}"),
            DomainEdit::Removed => write!(f, "removed {domain}"),
            DomainEdit::Repointed { to: None } => write!(f, "{domain} now follows PORT"),
            DomainEdit::Repointed { to: Some(port) } => {
                write!(f, "{domain} now reaches port {port}")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use ployz_core::config::ServiceSettingChange;
    use serde_json::{Value, json};

    use super::DomainChange;

    fn row(path: &str, before: Value, after: Value) -> ServiceSettingChange {
        serde_json::from_value(json!({
            "path": path,
            "kind": "update",
            "before": before,
            "after": after,
            "canRestore": false,
        }))
        .unwrap()
    }

    fn lines(row: &ServiceSettingChange) -> Vec<String> {
        DomainChange::of(row)
            .unwrap()
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    #[test]
    fn generated_domains_read_by_prefix() {
        let row = row(
            "web.managedHostnames",
            json!([{ "prefix": "web", "targetPort": null }, { "prefix": "old", "targetPort": 80 }]),
            json!([{ "prefix": "web", "targetPort": 3000 }, { "prefix": "shop", "targetPort": null }]),
        );
        assert_eq!(
            lines(&row),
            ["removed old", "added shop", "web now reaches port 3000"]
        );
    }

    #[test]
    fn a_custom_domain_reads_by_hostname() {
        let route = json!({ "id": "r1", "hostname": "app.example.com", "targetPort": 8080 });
        let added = row("web.routes.r1", Value::Null, route.clone());
        assert_eq!(lines(&added), ["added app.example.com on port 8080"]);
        let removed = row("web.routes.r1", route, Value::Null);
        assert_eq!(lines(&removed), ["removed app.example.com"]);
    }

    #[test]
    fn a_new_service_has_no_domains_before() {
        let row = row(
            "web.managedHostnames",
            Value::Null,
            json!([{ "prefix": "web", "targetPort": null }]),
        );
        assert_eq!(lines(&row), ["added web"]);
    }

    #[test]
    fn other_settings_are_no_domain() {
        let row = row("web.replicas", json!(1), json!(3));
        assert_eq!(DomainChange::of(&row), None);
    }
}
