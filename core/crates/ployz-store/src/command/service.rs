//! A Service's lifecycle in Working State: create, rename and remove, each staged
//! until a Deploy. A new Service's Node Introduction is captured in the same
//! transaction and never changes after. Its Private DNS name is fixed at creation:
//! a rename changes only the name paths and results address it by.

use ployz_core::config::{
    SavedServiceIntent, ServiceImageCredentials, ServiceSource, parse_service_config,
};
use ployz_core::{RpcError, ServiceName};
use serde::{Deserialize, Serialize};
use serde_json::json;
use ts_rs::TS;

use super::{Command, replayable};
use crate::Actor;
use crate::error;
use crate::id::ServiceId;
use crate::scope::{self, EnvironmentRef, EnvironmentSummary};
use crate::settings::{Apply, ServiceSetting, SettingPath, image_source};
use crate::storage::Tx;

/// Create a Service that runs `image`, or an empty one to give a source later. Its
/// name is also its Private DNS name, which a rename keeps.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct CreateService {
    /// The new Service's ID, also its lineage.
    pub id: ServiceId,
    /// The Environment to create it in.
    #[serde(default)]
    pub environment: EnvironmentRef,
    /// Its name, unique in the Environment.
    pub name: ServiceName,
    /// The container image it runs; none creates an empty Service.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
}

/// Rename a Service. Its Private DNS name, and references to it, stay as they are.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct RenameService {
    /// The Environment it is in.
    #[serde(default)]
    pub environment: EnvironmentRef,
    /// Its current name.
    pub service: ServiceName,
    /// Its new name, unique in the Environment.
    pub name: ServiceName,
}

/// Remove a Service from Working State. It keeps running until a Deploy removes it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct RemoveService {
    /// The Environment it is in.
    #[serde(default)]
    pub environment: EnvironmentRef,
    /// Its name.
    pub service: ServiceName,
}

/// A Service created, renamed or removed in Working State, staged until a Deploy.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct ServiceStaged {
    /// The Service, as it is named after the change.
    pub service: ServiceSummary,
    /// The Environment, at its revision after the change.
    pub environment: EnvironmentSummary,
    /// What waits for a Deploy: every Setting of a new Service, or the Service itself
    /// for a rename or removal. Empty when nothing changed.
    pub staged: Vec<SettingPath>,
    /// What took effect at once: never anything here.
    pub immediate: Vec<SettingPath>,
}

/// A Service as results name it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct ServiceSummary {
    /// Its durable identity.
    pub id: ServiceId,
    /// Its name, which Setting paths address it by.
    pub name: ServiceName,
    /// Its Private DNS name, fixed at creation.
    pub private_dns: ServiceName,
}

pub(crate) fn create_service(
    tx: &mut dyn Tx,
    who: &Actor,
    create: &CreateService,
) -> Result<ServiceStaged, RpcError> {
    let command = Command::CreateService(create.clone());
    replayable(tx, who, &command, |tx| {
        let source = match &create.image {
            Some(image) => image_source(image.clone(), ServiceImageCredentials::None)?,
            None => ServiceSource::Empty {
                version: 1,
                root_dir: "/".to_owned(),
            },
        };
        insert_service(
            tx,
            who,
            &create.id,
            &create.environment,
            &create.name,
            source,
        )
    })
}

/// Stage a new Service running `source` and capture its Node Introduction.
pub(crate) fn insert_service(
    tx: &mut dyn Tx,
    who: &Actor,
    id: &ServiceId,
    environment: &EnvironmentRef,
    name: &ServiceName,
    source: ServiceSource,
) -> Result<ServiceStaged, RpcError> {
    let mut environment = scope::lock(tx, who, environment)?;
    refuse_taken(&environment, name, None)?;
    let config = parse_service_config(json!({
        "version": 2,
        "source": source,
        "preDeployCommand": null,
        "startCommand": null,
        "healthcheck": { "type": "none" },
        "restartPolicy": "unless-stopped",
        "privateDns": name,
    }))
    .map_err(|error| {
        error::invalid(
            format!("{}: {}", error.path, error.message),
            json!({ "setting": error.path }),
        )
    })?
    .settings;
    let staged = ServiceSetting::ALL
        .into_iter()
        .filter(|setting| setting.applies(&config) && setting.apply() == Apply::Staged)
        .map(|setting| SettingPath::of(name, setting))
        .collect();
    let node = SavedServiceIntent {
        id: id.to_string(),
        // A new Service starts its own lineage; Branch copies keep it.
        lineage_id: id.to_string(),
        slug: name.to_string(),
        config,
        variables: Vec::new(),
        volume_attachments: Vec::new(),
    };
    environment.working.services.push(node.clone());
    scope::save_working(tx, &mut environment)?;
    scope::introduce(
        tx,
        who,
        &environment.summary.id,
        scope::Node::Service(&node),
    )?;
    Ok(ServiceStaged {
        service: summary(&node)?,
        environment: environment.summary,
        staged,
        immediate: Vec::new(),
    })
}

pub(crate) fn rename_service(
    tx: &mut dyn Tx,
    who: &Actor,
    rename: &RenameService,
) -> Result<ServiceStaged, RpcError> {
    let mut environment = scope::lock(tx, who, &rename.environment)?;
    let id = environment.service(&rename.service)?.id.clone();
    let changed = rename.name != rename.service;
    if changed {
        refuse_taken(&environment, &rename.name, Some(&id))?;
        environment.service_mut(&rename.service)?.slug = rename.name.to_string();
        scope::save_working(tx, &mut environment)?;
    }
    let service = summary(environment.service(&rename.name)?)?;
    Ok(ServiceStaged {
        staged: staged(changed, &service.name),
        service,
        environment: environment.summary,
        immediate: Vec::new(),
    })
}

pub(crate) fn remove_service(
    tx: &mut dyn Tx,
    who: &Actor,
    remove: &RemoveService,
) -> Result<ServiceStaged, RpcError> {
    let mut environment = scope::lock(tx, who, &remove.environment)?;
    let service = summary(environment.service(&remove.service)?)?;
    environment
        .working
        .services
        .retain(|node| node.id != service.id.as_str());
    scope::save_working(tx, &mut environment)?;
    Ok(ServiceStaged {
        staged: staged(true, &service.name),
        service,
        environment: environment.summary,
        immediate: Vec::new(),
    })
}

/// A name is taken by another Service's name or Private DNS name: either would make
/// `name` address two Services. `volumes` is never free.
fn refuse_taken(
    environment: &scope::Environment,
    name: &ServiceName,
    except: Option<&str>,
) -> Result<(), RpcError> {
    // `volumes.NAME` addresses a Volume, so no Service takes that name.
    if name.as_str() == "volumes" {
        return Err(error::invalid(
            "volumes is reserved: paths name Volumes as volumes.NAME",
            json!({ "service": name }),
        ));
    }
    let taken = environment.working.services.iter().any(|service| {
        Some(service.id.as_str()) != except
            && (service.slug == name.as_str() || service.config.private_dns == *name)
    });
    if taken {
        return Err(error::conflict(
            format!(
                "Environment {} already has a Service named {name}",
                environment.summary.name
            ),
            json!({ "service": name }),
        ));
    }
    Ok(())
}

fn staged(changed: bool, name: &ServiceName) -> Vec<SettingPath> {
    if changed {
        vec![SettingPath::whole(name)]
    } else {
        Vec::new()
    }
}

pub(crate) fn summary(node: &SavedServiceIntent) -> Result<ServiceSummary, RpcError> {
    Ok(ServiceSummary {
        id: ServiceId::parse(node.id.as_str()).map_err(|_| error::corrupt("Service ID"))?,
        name: ServiceName::parse(node.slug.as_str()).map_err(|_| error::corrupt("Service name"))?,
        private_dns: node.config.private_dns.clone(),
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
        let store =
            ConfigStore::open("sqlite::memory:", crate::SealingKey::new(b"test").unwrap()).unwrap();
        let who = Actor::system(OrganizationId::parse("org").unwrap());
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
                    image: Some("nginx:1".into()),
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
