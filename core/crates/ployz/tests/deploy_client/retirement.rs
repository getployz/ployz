//! A Deployment is applied once traffic moved; its old copies retire after.

use super::support::*;
use ployz::deploy::DeployWarning;
use ployz_store::{
    Actor, Admit, ConfigStore, CreateProject, CreateService, Deploy, DeploymentId, DeploymentQuery,
    DeploymentStatus, DeploymentSummary, DeploymentWarning, EnvironmentId, EnvironmentRef,
    OrganizationId, ProjectId, ProjectName, RunnerId, SealingKey, ServiceLineageId, Trusted,
};
use std::sync::Arc;

struct Shop {
    store: Arc<ConfigStore>,
    who: Actor,
    address: std::net::SocketAddr,
}

impl Shop {
    /// A shop whose web Service one Deployment already put on `service`'s Machine.
    async fn deployed(service: DeployService) -> Self {
        let store = Arc::new(
            ConfigStore::open("sqlite::memory:", SealingKey::new(b"cloud").unwrap()).unwrap(),
        );
        let who = Actor::system(OrganizationId::parse("org").unwrap());
        store
            .write(
                &who,
                &CreateProject {
                    id: ProjectId::parse("00000000-0000-4000-8000-000000000001").unwrap(),
                    name: ProjectName::parse("shop").unwrap(),
                    default_environment: EnvironmentId::parse(
                        "00000000-0000-4000-8000-000000000002",
                    )
                    .unwrap(),
                    region: None,
                },
            )
            .unwrap();
        store
            .write(
                &who,
                &CreateService {
                    id: ServiceLineageId::parse("00000000-0000-4000-8000-000000000003").unwrap(),
                    environment: EnvironmentRef::default(),
                    name: ployz_core::ServiceName::parse("web").unwrap(),
                    image: Some("nginx".into()),
                    template: None,
                },
            )
            .unwrap();
        let (address, _server) = listening(service).await;
        let shop = Self {
            store,
            who,
            address,
        };
        let first = shop.run(1, false).await.unwrap();
        assert_eq!(first.status, DeploymentStatus::Applied);
        shop
    }

    fn id(number: u8) -> DeploymentId {
        DeploymentId::parse(format!("00000000-0000-4000-8000-0000000001{number:02}")).unwrap()
    }

    /// Admit and run Deployment `number`; `recreate` replaces web's copy.
    fn run(
        &self,
        number: u8,
        recreate: bool,
    ) -> impl Future<Output = Result<DeploymentSummary, ployz_core::RpcError>> + 'static {
        let id = Self::id(number);
        self.store
            .write_trusted(
                &self.who,
                &Admit::Deploy(Deploy {
                    id: id.clone(),
                    environment: EnvironmentRef::default(),
                    services: Vec::new(),
                    version: None,
                    upload: None,
                    accept_volume_loss: Vec::new(),
                    message: None,
                    prune: false,
                    no_prune: false,
                    versions: Vec::new(),
                    copies: Vec::new(),
                    recreate: if recreate {
                        vec![ployz_core::ServiceName::parse("web").unwrap()]
                    } else {
                        Vec::new()
                    },
                    rule: None,
                }),
                &Trusted::default(),
            )
            .unwrap();
        ployz::sdk::run_deployment(
            ployz_core::Operator::Cloud,
            Arc::clone(&self.store),
            id,
            RunnerId::parse(format!("cloud-run-{number}")).unwrap(),
            vec![ployz::context::Connection::tcp(self.address)],
            Ok(Default::default()),
        )
    }

    fn status(&self, number: u8) -> DeploymentStatus {
        self.store
            .read(
                &self.who,
                &DeploymentQuery {
                    id: Self::id(number),
                },
            )
            .unwrap()
            .deployment
            .status
    }
}

#[tokio::test]
async fn a_deployment_is_applied_before_its_old_copy_is_removed() {
    let service = DeployService::new(machine('a', "one"))
        .lists_created()
        .hold_removals();
    let removed = service.removed_containers();
    let shop = Shop::deployed(service.clone()).await;

    let running = tokio::spawn(shop.run(2, true));
    service.removal_waited().await;
    // The runner records the outcome beside the removal; the removal never ends.
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while shop.status(2) != DeploymentStatus::Applied {
            // real-wait: the runner records in a blocking store call this test can't await.
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("applied while its old copy is still being removed");
    assert!(removed.lock().unwrap().is_empty());

    service.release_removals();
    let ran = running.await.unwrap().unwrap();
    assert_eq!(ran.status, DeploymentStatus::Applied);
    assert_eq!(removed.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn an_old_copy_that_will_not_go_is_a_warning_on_an_applied_deployment() {
    let service = DeployService::new(machine('a', "one"))
        .lists_created()
        .fail_removals(ployz_core::RpcError::internal("docker is busy"));
    let shop = Shop::deployed(service).await;

    let ran = shop.run(2, true).await.unwrap();

    assert_eq!(ran.status, DeploymentStatus::Applied);
    let left: Vec<_> = ran
        .warnings
        .iter()
        .filter(|warning| {
            matches!(
                warning,
                DeploymentWarning::Deploy {
                    warning: DeployWarning::LeftRunning { .. }
                }
            )
        })
        .collect();
    assert_eq!(left.len(), 1, "{:?}", ran.warnings);
}

#[tokio::test]
async fn stopping_old_runs_until_the_old_copy_retired_after_the_outcome_is_gone() {
    let service = DeployService::new(machine('a', "one"))
        .lists_created()
        .hold_removals();
    let shop = Shop::deployed(service.clone()).await;
    let stopping_old = |shop: &Shop| {
        shop.store
            .read(&shop.who, &DeploymentQuery { id: Shop::id(2) })
            .unwrap()
            .steps
            .into_iter()
            .find(|step| step.name == ployz_core::deploy_steps::StepName::StoppingOld)
    };

    let running = tokio::spawn(shop.run(2, true));
    service.removal_waited().await;
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while shop.status(2) != DeploymentStatus::Applied {
            // real-wait: the runner records in a blocking store call this test can't await.
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("applied while its old copy is still being removed");
    let open = stopping_old(&shop).expect("stopping_old started at the switch");
    assert_eq!(open.ended_at, None, "the old copy is still being removed");

    service.release_removals();
    running.await.unwrap().unwrap();
    let ended = stopping_old(&shop).expect("stopping_old");
    assert!(ended.ended_at.is_some(), "{ended:?}");
}
