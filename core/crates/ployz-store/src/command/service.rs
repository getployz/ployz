//! Creating Services. A new Service is staged in Working State, and its Node
//! Introduction is captured in the same transaction and never changes after.

use ployz_core::config::{SavedServiceIntent, ServiceImageCredentials, parse_service_config};
use ployz_core::{RpcError, ServiceName};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{Command, replayable};
use crate::Actor;
use crate::error;
use crate::id::ServiceId;
use crate::scope::{self, EnvironmentRef, EnvironmentSummary};
use crate::settings::{ServiceSetting, SettingPath, image_source};
use crate::storage::Tx;

/// Create a Service that runs `image`. Its name is its Private DNS name.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateService {
    /// The new Service's ID, also its lineage.
    pub id: ServiceId,
    /// The Environment to create it in.
    #[serde(default)]
    pub environment: EnvironmentRef,
    /// Its name, unique in the Environment.
    pub name: ServiceName,
    /// The container image it runs.
    pub image: String,
}

/// The new Service, staged in Working State until a Deploy.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ServiceCreated {
    /// The Service.
    pub service: ServiceSummary,
    /// The Environment, at its revision after the create.
    pub environment: EnvironmentSummary,
    /// Every Setting of the new Service, waiting for a Deploy.
    pub staged: Vec<SettingPath>,
    /// Settings that took effect at once: none for a new Service.
    pub immediate: Vec<SettingPath>,
}

/// A Service as results name it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ServiceSummary {
    /// Its durable identity.
    pub id: ServiceId,
    /// Its name, also its Private DNS name.
    pub name: ServiceName,
}

pub(crate) fn create_service(
    tx: &mut dyn Tx,
    who: &Actor,
    create: &CreateService,
) -> Result<ServiceCreated, RpcError> {
    let command = Command::CreateService(create.clone());
    replayable(tx, who, &command, |tx| insert_service(tx, who, create))
}

fn insert_service(
    tx: &mut dyn Tx,
    who: &Actor,
    create: &CreateService,
) -> Result<ServiceCreated, RpcError> {
    let mut environment = scope::lock(tx, who, &create.environment)?;
    if environment.service(&create.name).is_ok() {
        return Err(error::conflict(
            format!(
                "Environment {} already has a Service named {}",
                environment.summary.name, create.name
            ),
            json!({ "service": create.name }),
        ));
    }
    let source = image_source(create.image.clone(), ServiceImageCredentials::None)?;
    let config = parse_service_config(json!({
        "version": 2,
        "source": source,
        "preDeployCommand": null,
        "startCommand": null,
        "healthcheck": { "type": "none" },
        "restartPolicy": "unless-stopped",
        "privateDns": create.name,
    }))
    .map_err(|error| {
        error::invalid(
            format!("image: {}", error.message),
            json!({ "setting": "image" }),
        )
    })?
    .settings;
    let node = SavedServiceIntent {
        id: create.id.to_string(),
        // A new Service starts its own lineage; Branch copies keep it.
        lineage_id: create.id.to_string(),
        slug: create.name.to_string(),
        config,
        variables: Vec::new(),
        volume_attachments: Vec::new(),
    };
    environment.working.services.push(node.clone());
    scope::save_working(tx, &mut environment)?;
    tx.execute(
        "INSERT INTO config_node_introduction \
         (environment_id, node_id, organization_id, node_type, node) \
         VALUES (?1, ?2, ?3, 'service', ?4)",
        &[
            environment.summary.id.as_str().into(),
            create.id.as_str().into(),
            who.organization.as_str().into(),
            serde_json::to_string(&node)
                .expect("a Service node is JSON")
                .as_str()
                .into(),
        ],
    )?;
    Ok(ServiceCreated {
        service: ServiceSummary {
            id: create.id.clone(),
            name: create.name.clone(),
        },
        environment: environment.summary,
        staged: ServiceSetting::ALL
            .into_iter()
            .map(|setting| SettingPath::of(&create.name, setting))
            .collect(),
        immediate: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use crate::{
        Actor, Change, ConfigStore, CreateProject, CreateService, Edit, EnvironmentId,
        EnvironmentRef, OrganizationId, ProjectId, ProjectName, ServiceId, SettingPath,
    };

    /// Node Introductions have no read yet (Discard uses them), so this reads the row.
    #[test]
    fn a_node_introduction_keeps_the_service_as_created() {
        let store = ConfigStore::open("sqlite::memory:").unwrap();
        let who = Actor {
            organization: OrganizationId::parse("org").unwrap(),
        };
        let uuid = |n: u8| format!("00000000-0000-4000-8000-00000000000{n}");
        store
            .create_project(
                &who,
                &CreateProject {
                    id: ProjectId::parse(uuid(1)).unwrap(),
                    name: ProjectName::parse("shop").unwrap(),
                    default_environment: EnvironmentId::parse(uuid(2)).unwrap(),
                },
            )
            .unwrap();
        store
            .create_service(
                &who,
                &CreateService {
                    id: ServiceId::parse(uuid(3)).unwrap(),
                    environment: EnvironmentRef::default(),
                    name: ployz_core::ServiceName::parse("web").unwrap(),
                    image: "nginx:1".into(),
                },
            )
            .unwrap();
        store
            .edit(
                &who,
                &Edit {
                    environment: EnvironmentRef::default(),
                    expect: None,
                    changes: vec![Change::Set {
                        path: SettingPath::parse("web.replicas").unwrap(),
                        value: json!(4),
                    }],
                },
            )
            .unwrap();
        let introduction: Value = store
            .storage
            .read(|tx| {
                let rows = tx.query("SELECT node FROM config_node_introduction", &[])?;
                Ok(serde_json::from_str(rows.first().unwrap().text(0)?).unwrap())
            })
            .unwrap();
        assert_eq!(introduction.pointer("/config/replicas"), Some(&json!(1)));
        assert_eq!(
            introduction.pointer("/config/source/image"),
            Some(&json!("nginx:1"))
        );
    }
}
