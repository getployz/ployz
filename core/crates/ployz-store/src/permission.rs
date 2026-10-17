//! The permissions Cloud grants a member by role, named as `resource:level`. The
//! Store never decides who holds one: Cloud passes the caller's set in [`crate::Trusted`],
//! and every command names the one it needs.

use std::fmt;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

macro_rules! permissions {
    ($($variant:ident = $name:literal,)*) => {
        /// One permission, as Cloud's `permissions.ts` names it.
        #[derive(
            Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize, TS,
        )]
        #[ts(rename = "ConfigPermission")]
        pub enum Permission {
            $(#[serde(rename = $name)] $variant,)*
        }

        impl Permission {
            /// Every permission, in the order Cloud lists them.
            pub const ALL: &'static [Self] = &[$(Self::$variant,)*];

            /// The `resource:level` name Cloud sends it as.
            #[must_use]
            pub const fn name(self) -> &'static str {
                match self {
                    $(Self::$variant => $name,)*
                }
            }
        }
    };
}

permissions! {
    OrgRead = "org:read",
    OrgSettings = "org:settings",
    OrgBilling = "org:billing",
    OrgDelete = "org:delete",
    MembersRead = "members:read",
    MembersManage = "members:manage",
    TokensCreate = "tokens:create",
    TokensManage = "tokens:manage",
    AgentsManage = "agents:manage",
    GithubConnect = "github:connect",
    ApprovalsAnswer = "approvals:answer",
    AuditRead = "audit:read",
    ProjectRead = "project:read",
    ProjectCreate = "project:create",
    ProjectEdit = "project:edit",
    ProjectDelete = "project:delete",
    EnvironmentCreate = "environment:create",
    EnvironmentDelete = "environment:delete",
    EnvironmentsProtect = "environments:protect",
    ChangesStage = "changes:stage",
    ChangesDeploy = "changes:deploy",
    ChangesDestroy = "changes:destroy",
    ChangesMerge = "changes:merge",
    ServicesControl = "services:control",
    VariablesRead = "variables:read",
    LogsRead = "logs:read",
    MetricsRead = "metrics:read",
    VolumesRead = "volumes:read",
    VolumesDeleteCopy = "volumes:delete-copy",
    ServersRead = "servers:read",
    ServersAdd = "servers:add",
    ServersOperate = "servers:operate",
    ServersRemove = "servers:remove",
    ServersShell = "servers:shell",
}

impl fmt::Display for Permission {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::indexing_slicing,
        reason = "the fixture is fixed; a missing entry must fail the test"
    )]

    use serde_json::Value;

    use super::Permission;

    const MANIFEST: &str = include_str!("../../ployz/tests/fixtures/published-manifest.json");

    #[test]
    fn names_round_trip_through_serde() {
        for permission in Permission::ALL {
            let json = serde_json::to_value(permission).unwrap();
            assert_eq!(json, Value::String(permission.name().to_owned()));
            assert_eq!(
                serde_json::from_value::<Permission>(json).unwrap(),
                *permission
            );
        }
    }

    #[test]
    fn matches_the_permissions_cloud_publishes() {
        let manifest: Value = serde_json::from_str(MANIFEST).unwrap();
        let published = manifest["permissions"]
            .as_array()
            .expect("the manifest lists Cloud's permissions")
            .iter()
            .map(|name| name.as_str().unwrap())
            .collect::<Vec<_>>();
        let known = Permission::ALL
            .iter()
            .map(|permission| permission.name())
            .collect::<Vec<_>>();
        assert_eq!(
            published, known,
            "permission.rs and dashboard permissions.ts disagree: run `pnpm manifest:fixture` in dashboard/ and keep both lists in step"
        );
    }

    #[test]
    fn every_published_command_requires_a_known_permission() {
        let manifest: Value = serde_json::from_str(MANIFEST).unwrap();
        for command in manifest["commands"].as_array().unwrap() {
            let name = command["name"].as_str().unwrap();
            let requires = command["requires"].clone();
            assert!(
                serde_json::from_value::<Permission>(requires.clone()).is_ok(),
                "{name} requires {requires}, which permission.rs does not know"
            );
        }
    }
}
