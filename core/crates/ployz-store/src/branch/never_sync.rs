//! Never sync: settings an Environment keeps as its own. A Sync never carries one
//! from that Environment nor changes it there, but a Branch of it still gets its
//! value: the mark doesn't carry into children.

use super::*;
use crate::settings::Target;

/// Mark settings of an Environment Never sync, or sync them again.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct NeverSync {
    /// The Environment.
    #[serde(default)]
    pub environment: EnvironmentRef,
    /// Its settings: `SERVICE.SETTING`, `SERVICE.env.KEY`, `SERVICE.mounts.VOLUME`,
    /// or `volumes.VOLUME.name` / `volumes.VOLUME.storage`.
    pub paths: Vec<SettingPath>,
    /// Sync them again.
    #[serde(default)]
    #[ts(as = "Option<bool>", optional)]
    pub off: bool,
}

/// An Environment's settings marked Never sync, after a change.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct NeverSynced {
    /// The Environment.
    pub environment: EnvironmentSummary,
    /// Every setting it marks Never sync.
    pub never_synced: Vec<SettingPath>,
}

/// A setting an Environment marked Never sync, by the row key a comparison names
/// it by; it covers the rows under it too (`healthcheck` covers `healthcheck.path`).
pub(crate) struct Mark {
    pub(crate) environment: EnvironmentId,
    pub(crate) key: String,
}

pub(crate) fn never_sync(
    tx: &mut dyn Tx,
    who: &Actor,
    request: &NeverSync,
) -> Result<NeverSynced, RpcError> {
    let environment = scope::lock(tx, who, &request.environment)?;
    for path in &request.paths {
        let (lineage, field) = row_of(&environment.working, path)?;
        let at = [
            environment.summary.id.as_str().into(),
            lineage.as_str().into(),
            field.as_str().into(),
        ];
        if request.off {
            tx.execute(
                "DELETE FROM config_never_sync \
                 WHERE environment_id = ?1 AND lineage = ?2 AND path = ?3",
                &at,
            )?;
        } else {
            let [id, lineage, field] = at;
            tx.execute(
                "INSERT INTO config_never_sync (environment_id, lineage, path, organization_id) \
                 VALUES (?1, ?2, ?3, ?4) \
                 ON CONFLICT (environment_id, lineage, path) DO NOTHING",
                &[id, lineage, field, who.organization.as_str().into()],
            )?;
        }
    }
    Ok(NeverSynced {
        never_synced: never_synced(tx, &environment)?,
        environment: environment.summary,
    })
}

/// The settings `environment` marks Never sync, by path; a mark on a node it no
/// longer has is left out.
pub(crate) fn never_synced(
    tx: &mut dyn Tx,
    environment: &Environment,
) -> Result<Vec<SettingPath>, RpcError> {
    let rows = tx.query(
        "SELECT lineage, path FROM config_never_sync WHERE environment_id = ?1",
        &[environment.summary.id.as_str().into()],
    )?;
    let intent = &environment.working;
    let mut paths = Vec::new();
    for row in &rows {
        let (lineage, field) = (row.text(0)?, row.text(1)?);
        let Some(node) = node_of(intent, lineage) else {
            continue;
        };
        let path = match &node {
            NodeName::Volume(_) => format!("{node}.{field}"),
            NodeName::Service(service) => SettingPath::from_core(service.as_str(), field, |id| {
                name_of(intent, id).unwrap_or_else(|| id.to_owned())
            }),
        };
        paths.extend(SettingPath::parse(&path).ok());
    }
    paths.sort_by_key(ToString::to_string);
    Ok(paths)
}

/// What a move from `from` into `into` never carries: what `into` marked, and what
/// `from` marked unless `into` is one of its Branches.
pub(crate) fn marks(
    tx: &mut dyn Tx,
    from: &EnvironmentId,
    into: &EnvironmentId,
) -> Result<Vec<Mark>, RpcError> {
    tx.query(
        "SELECT environment_id, lineage, path FROM config_never_sync \
         WHERE environment_id = ?2 OR (environment_id = ?1 AND NOT EXISTS ( \
             SELECT 1 FROM config_environment_branch WHERE environment_id = ?2 AND parent_id = ?1))",
        &[from.as_str().into(), into.as_str().into()],
    )?
    .iter()
    .map(|row| {
        Ok(Mark {
            environment: row.parse::<EnvironmentId>(0, "Never sync")?,
            key: format!("{}:{}", row.text(1)?, row.text(2)?),
        })
    })
    .collect()
}

/// The lineage and comparison path of the setting `path` names in `intent`.
fn row_of(
    intent: &SavedEnvironmentIntent,
    path: &SettingPath,
) -> Result<(String, String), RpcError> {
    let lineage = lineage_named(intent, &path.node())?;
    let field = match (path.target(), path.volume_field()) {
        (Some(Target::Setting(setting)), _) => setting.field().to_owned(),
        (Some(Target::Variable(key) | Target::Exported(key)), _) => format!("variables.{key}"),
        (Some(Target::Mount(volume)), _) => {
            let volume = lineage_named(intent, &NodeName::Volume(volume.clone()))?;
            format!("mounts.{volume}")
        }
        (None, Some(field)) => field.name().to_owned(),
        (None, None) => return Err(crate::settings::name_a_setting(path.node())),
    };
    Ok((lineage, field))
}
