//! The Service Settings a row can address, one table: where each is, whether a new node
//! carries it, who owns it, its default, what its cell holds and how a cell is put back.

use serde_json::{Value, json};

use super::Why;
use crate::config::{
    AuthoredServiceConfig, BuildMethod, ServiceGitBranch, ServiceHealthcheck,
    ServiceImageCredentials, ServiceRestartPolicy, ServiceSource, default_max_retries,
    default_replicas,
};

/// A Service setting a row can address. Declaration order is landing order, and row `i`
/// of the table is variant `i`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Setting {
    Source,
    Branch,
    PrivateDns,
    ManagedHostnames,
    Routes,
    PreDeployCommand,
    StartCommand,
    Healthcheck,
    RestartPolicy,
    MaxRetries,
    UpdateOrder,
    Replicas,
    CpuLimit,
    MemLimit,
    BuildMethod,
    DockerfilePath,
    BuildCommand,
}

type Write = fn(&mut AuthoredServiceConfig, &str, Value) -> Result<(), serde_json::Error>;

/// What one Setting is. A cell is a lossy projection: [`read`](Self::read) is not a
/// getter, so nothing that restores state goes through it.
struct Def {
    setting: Setting,
    /// Where it is in a Service's configuration, as the review names it. Frozen: a row
    /// id and a Never sync mark persist it.
    path: &'static str,
    /// Whether a new node arrives with it on its node row rather than as a row.
    carried: bool,
    /// Why each Environment keeps its own, if it does.
    owner: Option<Why>,
    default: fn() -> Value,
    /// Its cell: the setting as a plan compares it, with generated addresses stripped of
    /// `suffix`.
    read: fn(&AuthoredServiceConfig, suffix: &str) -> Value,
    /// Puts a cell back into the config of the Service with this id; none if it never
    /// moves.
    write: Option<Write>,
}

const TABLE: &[Def; 17] = &[
    Def {
        setting: Setting::Source,
        path: "source",
        carried: true,
        owner: None,
        default: || json!(ServiceSource::empty()),
        read: |config, _| source_cell(&config.source),
        write: Some(|config, id, value| {
            config.source = keep_branch(&config.source, source_of(id, value, Value::Null)?);
            Ok(())
        }),
    },
    Def {
        setting: Setting::Branch,
        path: "source.branch",
        carried: true,
        owner: Some(Why::GitBranch),
        default: || Value::Null,
        read: |config, _| match &config.source {
            ServiceSource::Git { branch, .. } => json!(branch),
            ServiceSource::Empty { .. } | ServiceSource::Image { .. } => Value::Null,
        },
        write: Some(|config, _, value| {
            if let ServiceSource::Git { branch, .. } = &mut config.source
                && !value.is_null()
            {
                *branch = serde_json::from_value(value)?;
            }
            Ok(())
        }),
    },
    Def {
        setting: Setting::PrivateDns,
        path: "privateDns",
        carried: true,
        owner: None,
        default: || Value::Null,
        read: |config, _| json!(config.private_dns),
        write: Some(|config, _, value| {
            config.private_dns = serde_json::from_value(value)?;
            Ok(())
        }),
    },
    Def {
        setting: Setting::ManagedHostnames,
        path: "managedHostnames",
        carried: true,
        owner: Some(Why::GeneratedAddress),
        default: || Value::Null,
        read: |config, suffix| {
            json!(
                config
                    .managed_hostnames
                    .iter()
                    .map(|h| json!({"prefix": h.prefix.strip_suffix(suffix).unwrap_or(&h.prefix), "targetPort": h.target_port}))
                    .collect::<Vec<_>>()
            )
        },
        write: None,
    },
    Def {
        setting: Setting::Routes,
        path: "routes",
        carried: true,
        owner: Some(Why::CustomDomain),
        default: || Value::Null,
        read: |config, _| {
            let mut domains: Vec<_> = config.routes.iter().map(|r| &r.hostname).collect();
            domains.sort();
            json!(domains)
        },
        write: None,
    },
    Def {
        setting: Setting::PreDeployCommand,
        path: "preDeployCommand",
        carried: false,
        owner: None,
        default: || Value::Null,
        read: |config, _| json!(config.pre_deploy_command),
        write: Some(|config, _, value| {
            config.pre_deploy_command = serde_json::from_value(value)?;
            Ok(())
        }),
    },
    Def {
        setting: Setting::StartCommand,
        path: "startCommand",
        carried: false,
        owner: None,
        default: || Value::Null,
        read: |config, _| json!(config.start_command),
        write: Some(|config, _, value| {
            config.start_command = serde_json::from_value(value)?;
            Ok(())
        }),
    },
    Def {
        setting: Setting::Healthcheck,
        path: "healthcheck",
        carried: false,
        owner: None,
        default: || json!(ServiceHealthcheck::default()),
        read: |config, _| json!(config.healthcheck),
        write: Some(|config, _, value| {
            config.healthcheck = serde_json::from_value(value)?;
            Ok(())
        }),
    },
    Def {
        setting: Setting::RestartPolicy,
        path: "restartPolicy",
        carried: false,
        owner: None,
        default: || json!(ServiceRestartPolicy::default()),
        read: |config, _| json!(config.restart_policy),
        write: Some(|config, _, value| {
            config.restart_policy = serde_json::from_value(value)?;
            Ok(())
        }),
    },
    Def {
        setting: Setting::MaxRetries,
        path: "maxRetries",
        carried: false,
        owner: None,
        default: || json!(default_max_retries()),
        read: |config, _| json!(config.max_retries),
        write: Some(|config, _, value| {
            config.max_retries = serde_json::from_value(value)?;
            Ok(())
        }),
    },
    Def {
        setting: Setting::UpdateOrder,
        path: "updateOrder",
        carried: false,
        owner: None,
        default: || Value::Null,
        read: |config, _| json!(config.update_order),
        write: Some(|config, _, value| {
            config.update_order = serde_json::from_value(value)?;
            Ok(())
        }),
    },
    Def {
        setting: Setting::Replicas,
        path: "replicas",
        carried: false,
        owner: Some(Why::Sizing),
        default: || json!(default_replicas()),
        read: |config, _| json!(config.replicas),
        write: Some(|config, _, value| {
            config.replicas = serde_json::from_value(value)?;
            Ok(())
        }),
    },
    Def {
        setting: Setting::CpuLimit,
        path: "cpuLimit",
        carried: false,
        owner: Some(Why::Sizing),
        default: || Value::Null,
        read: |config, _| json!(config.cpu_limit),
        write: Some(|config, _, value| {
            config.cpu_limit = serde_json::from_value(value)?;
            Ok(())
        }),
    },
    Def {
        setting: Setting::MemLimit,
        path: "memLimit",
        carried: false,
        owner: Some(Why::Sizing),
        default: || Value::Null,
        read: |config, _| json!(config.mem_limit),
        write: Some(|config, _, value| {
            config.mem_limit = serde_json::from_value(value)?;
            Ok(())
        }),
    },
    Def {
        setting: Setting::BuildMethod,
        path: "build.buildMethod",
        carried: false,
        owner: None,
        default: || json!(BuildMethod::default()),
        read: |config, _| json!(config.build.build_method),
        write: Some(|config, _, value| {
            config.build.build_method = serde_json::from_value(value)?;
            Ok(())
        }),
    },
    Def {
        setting: Setting::DockerfilePath,
        path: "build.dockerfilePath",
        carried: false,
        owner: None,
        default: || Value::Null,
        read: |config, _| json!(config.build.dockerfile_path),
        write: Some(|config, _, value| {
            config.build.dockerfile_path = serde_json::from_value(value)?;
            Ok(())
        }),
    },
    Def {
        setting: Setting::BuildCommand,
        path: "build.command",
        carried: false,
        owner: None,
        default: || Value::Null,
        read: |config, _| json!(config.build.command),
        write: Some(|config, _, value| {
            config.build.command = serde_json::from_value(value)?;
            Ok(())
        }),
    },
];

impl Setting {
    /// Every Setting, in landing order. Building it proves row `i` of the table is
    /// variant `i`, which is how [`def`](Self::def) finds a row.
    #[expect(
        clippy::indexing_slicing,
        reason = "A const loop bounded by the table's length; the assertion is the check."
    )]
    pub const ALL: [Self; 17] = {
        let mut all = [Self::Source; 17];
        let mut row = 0;
        while row < all.len() {
            assert!(
                TABLE[row].setting as usize == row,
                "row i of the table is Setting variant i"
            );
            all[row] = TABLE[row].setting;
            row += 1;
        }
        all
    };

    #[expect(
        clippy::indexing_slicing,
        reason = "Row i is variant i, which building ALL asserts."
    )]
    const fn def(self) -> &'static Def {
        &TABLE[self as usize]
    }

    /// Where it is in a Service's configuration, as the review names it.
    #[must_use]
    pub const fn path(self) -> &'static str {
        self.def().path
    }

    pub(super) fn of_path(path: &str) -> Option<Self> {
        TABLE
            .iter()
            .find(|def| def.path == path)
            .map(|def| def.setting)
    }

    /// Whether a new node arrives with it on its node row rather than as a row.
    pub(super) const fn carried(self) -> bool {
        self.def().carried
    }

    /// Why each Environment keeps its own, if it does.
    pub(super) const fn owner(self) -> Option<Why> {
        self.def().owner
    }

    /// What it holds when nothing sets it; `null` for a setting that may be unset.
    pub(in crate::config) fn default(self) -> Value {
        (self.def().default)()
    }

    /// Its cell in `config`: normalized so copies compare with their originals, with
    /// generated addresses stripped of `suffix`.
    pub(in crate::config) fn read(self, config: &AuthoredServiceConfig, suffix: &str) -> Value {
        (self.def().read)(config, suffix)
    }

    /// Put `value` into the `config` of the Service with this `id`.
    ///
    /// # Errors
    /// When `value` is not one this setting takes, or the setting never moves.
    pub(super) fn write(
        self,
        config: &mut AuthoredServiceConfig,
        id: &str,
        value: Value,
    ) -> Result<(), serde_json::Error> {
        let write = self
            .def()
            .write
            .ok_or_else(|| serde::de::Error::custom("never moves"))?;
        write(config, id, value)
    }
}

/// A source as its row holds it: all of it but its git branch, its credentials by
/// presence.
pub(in crate::config) fn source_cell(source: &ServiceSource) -> Value {
    let mut cell = json!(source);
    let fields = cell.as_object_mut().expect("a source is an object");
    fields.remove("branch");
    if let ServiceSource::Image { credentials, .. } = source {
        let configured = matches!(credentials, ServiceImageCredentials::Configured { .. });
        fields.insert("credentials".into(), json!(configured));
    }
    cell
}

/// `source` as a Service on `current` takes it: the git branch is the Service's own, so
/// it keeps the one it has, else the one `source` carries.
pub(in crate::config) fn keep_branch(
    current: &ServiceSource,
    mut source: ServiceSource,
) -> ServiceSource {
    if let (ServiceSource::Git { branch: own, .. }, ServiceSource::Git { branch, .. }) =
        (current, &mut source)
    {
        branch.clone_from(own);
    }
    source
}

/// The source a Service with ID `id` holds as its `source` cell and its
/// `source.branch` cell say.
pub(super) fn source_of(
    id: &str,
    mut source: Value,
    branch: Value,
) -> Result<ServiceSource, serde_json::Error> {
    let fields = source
        .as_object_mut()
        .ok_or_else(|| serde::de::Error::custom("not a source"))?;
    // A git source parses with the branch it carries, else none yet.
    if fields.get("type") == Some(&json!("git")) {
        if !branch.is_null() {
            fields.insert("branch".to_owned(), branch);
        }
        fields.entry("branch").or_insert_with(|| {
            json!(ServiceGitBranch::Disconnected {
                previous_name: None
            })
        });
    }
    // Each Service's credential is its own: it is bound to the Service's id.
    if let Some(credentials) = fields.get_mut("credentials") {
        *credentials = match serde_json::from_value(credentials.take())? {
            true => json!(ServiceImageCredentials::Configured {
                credential_id: id.to_owned()
            }),
            false => json!(ServiceImageCredentials::None),
        };
    }
    serde_json::from_value(source)
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;
    use crate::config::{At, RowId, parse_service_config};

    /// The paths a row id and a Never sync mark have stored; they never change.
    const PATHS: [&str; 17] = [
        "source",
        "source.branch",
        "privateDns",
        "managedHostnames",
        "routes",
        "preDeployCommand",
        "startCommand",
        "healthcheck",
        "restartPolicy",
        "maxRetries",
        "updateOrder",
        "replicas",
        "cpuLimit",
        "memLimit",
        "build.buildMethod",
        "build.dockerfilePath",
        "build.command",
    ];

    #[test]
    fn paths_are_frozen_in_landing_order_and_round_trip_through_a_row_id() {
        assert_eq!(Setting::ALL.map(Setting::path), PATHS);
        for (setting, path) in Setting::ALL.into_iter().zip(PATHS) {
            let id = RowId::from_str(&format!("lineage:{path}")).unwrap();
            assert_eq!(*id.at(), At::Setting(setting));
            assert_eq!(id.to_string(), format!("lineage:{path}"));
            assert_eq!(Setting::of_path(path), Some(setting));
        }
        assert_eq!(Setting::of_path("nonsense"), None);
    }

    #[test]
    fn defaults_serialize_as_the_literals_they_replace() {
        let expected = [
            (
                Setting::Source,
                r#"{"rootDir":"/","type":"empty","version":1}"#,
            ),
            (Setting::Healthcheck, r#"{"type":"none"}"#),
            (Setting::RestartPolicy, r#""unless-stopped""#),
            (Setting::MaxRetries, "10"),
            (Setting::Replicas, "1"),
            (Setting::BuildMethod, r#""railpack""#),
        ];
        for setting in Setting::ALL {
            let literal = expected
                .iter()
                .find_map(|(s, literal)| (*s == setting).then_some(*literal))
                .unwrap_or("null");
            assert_eq!(setting.default().to_string(), literal, "{setting:?}");
        }
    }

    fn authored() -> AuthoredServiceConfig {
        parse_service_config(json!({
            "version": 2, "privateDns": "api",
            "source": {"version": 2, "type": "git", "repository": "acme/api",
                "repositoryId": 42, "access": {"type": "github-installation", "installationId": 7},
                "rootDir": "/apps/api", "branch": {"type": "connected", "name": "main"}},
            "preDeployCommand": "migrate", "startCommand": "serve",
            "healthcheck": {"type": "http", "path": "/up", "timeoutSeconds": 5},
            "restartPolicy": "always", "maxRetries": 3, "updateOrder": "rolling",
            "replicas": 2, "cpuLimit": 0.5, "memLimit": 256,
            "build": {"buildMethod": "dockerfile", "dockerfilePath": "Dockerfile.prod", "command": "make"},
            "routes": [{"id": "00000000-0000-4000-8000-000000000001", "hostname": "b.example.com", "targetPort": null},
                {"id": "00000000-0000-4000-8000-000000000002", "hostname": "a.example.com", "targetPort": 80}],
            "managedHostnames": [{"prefix": "api-prod", "targetPort": 8080}]
        }))
        .unwrap()
        .settings
    }

    #[test]
    fn a_cell_is_the_setting_as_authored_but_for_the_three_that_project() {
        let config = authored();
        let serialized = json!(config);
        let projected = [Setting::Source, Setting::ManagedHostnames, Setting::Routes];
        for setting in Setting::ALL.into_iter().filter(|s| !projected.contains(s)) {
            let authored = setting.path().split('.').fold(&serialized, |value, key| {
                value.get(key).unwrap_or(&Value::Null)
            });
            assert_eq!(setting.read(&config, ""), *authored, "{setting:?}");
        }
        assert!(
            setting_cell(&config, Setting::Source)
                .get("branch")
                .is_none()
        );
        assert_eq!(
            setting_cell(&config, Setting::Routes),
            json!(["a.example.com", "b.example.com"])
        );
        assert_eq!(
            Setting::ManagedHostnames.read(&config, "-prod"),
            json!([{"prefix": "api", "targetPort": 8080}])
        );
    }

    fn setting_cell(config: &AuthoredServiceConfig, setting: Setting) -> Value {
        setting.read(config, "")
    }

    #[test]
    fn writing_a_cell_back_is_the_inverse_of_reading_it() {
        let config = authored();
        for setting in Setting::ALL {
            let mut copy = config.clone();
            let cell = setting.read(&config, "");
            if setting.def().write.is_none() {
                assert!(setting.write(&mut copy, "id", cell).is_err(), "{setting:?}");
            } else {
                setting.write(&mut copy, "id", cell).unwrap();
                assert_eq!(copy, config, "{setting:?}");
            }
        }
    }
}
