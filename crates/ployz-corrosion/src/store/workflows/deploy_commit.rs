use crate::client::CorrClient;
use crate::store::shared::sql::exec_all;
use crate::store::tables::{deploys, service_releases};
use corro_api_types::Statement;
use ployz_types::error::Result;
use ployz_types::model::{DeployRecord, ServiceReleaseRecord};
use ployz_types::spec::Namespace;

pub(crate) async fn commit_deploy(
    client: &CorrClient,
    namespace: &Namespace,
    removed_services: &[String],
    releases: &[ServiceReleaseRecord],
    deploy: &DeployRecord,
) -> Result<()> {
    let statements = build_commit_statements(namespace, removed_services, releases, deploy)?;
    exec_all(client, &statements, "commit_deploy").await
}

fn touched_services(removed_services: &[String], releases: &[ServiceReleaseRecord]) -> Vec<String> {
    let mut touched = removed_services.to_vec();
    for release in releases {
        if !touched.contains(&release.service) {
            touched.push(release.service.clone());
        }
    }
    touched
}

fn build_commit_statements(
    namespace: &Namespace,
    removed_services: &[String],
    releases: &[ServiceReleaseRecord],
    deploy: &DeployRecord,
) -> Result<Vec<Statement>> {
    let touched = touched_services(removed_services, releases);
    let mut statements = Vec::new();

    for service in &touched {
        statements.push(service_releases::delete_statement(namespace, service));
    }

    for release in releases {
        statements.push(service_releases::upsert_statement(release)?);
    }

    statements.push(deploys::upsert_statement(deploy)?);

    Ok(statements)
}

#[cfg(test)]
mod tests {
    use super::build_commit_statements;
    use corro_api_types::Statement;
    use ployz_types::model::{
        DeployId, DeployRecord, DeployState, MachineId, ServiceRelease, ServiceReleaseRecord,
        ServiceRoutingPolicy,
    };
    use ployz_types::spec::Namespace;

    #[test]
    fn build_commit_statements_deduplicates_services_and_preserves_order() {
        let namespace = Namespace(String::from("ns"));
        let removed_services = vec![String::from("api")];
        let releases = vec![
            ServiceReleaseRecord {
                namespace: namespace.clone(),
                service: String::from("api"),
                release: ServiceRelease {
                    primary_revision_hash: String::from("rev-api"),
                    referenced_revision_hashes: vec![String::from("rev-api")],
                    routing: ServiceRoutingPolicy::Direct {
                        revision_hash: String::from("rev-api"),
                    },
                    slots: Vec::new(),
                    updated_by_deploy_id: DeployId(String::from("deploy-1")),
                    updated_at: 10,
                },
            },
            ServiceReleaseRecord {
                namespace: namespace.clone(),
                service: String::from("worker"),
                release: ServiceRelease {
                    primary_revision_hash: String::from("rev-worker"),
                    referenced_revision_hashes: vec![String::from("rev-worker")],
                    routing: ServiceRoutingPolicy::Direct {
                        revision_hash: String::from("rev-worker"),
                    },
                    slots: Vec::new(),
                    updated_by_deploy_id: DeployId(String::from("deploy-1")),
                    updated_at: 11,
                },
            },
        ];
        let deploy = DeployRecord {
            deploy_id: DeployId(String::from("deploy-1")),
            namespace: namespace.clone(),
            coordinator_machine_id: MachineId(String::from("machine-1")),
            manifest_hash: String::from("manifest-1"),
            state: DeployState::Committed,
            started_at: 1,
            committed_at: Some(2),
            finished_at: Some(3),
            summary_json: String::from("{}"),
        };

        let statements = build_commit_statements(&namespace, &removed_services, &releases, &deploy)
            .expect("commit statements");

        let [
            delete_api_release,
            delete_worker_release,
            upsert_api_release,
            upsert_worker_release,
            upsert_deploy,
        ] = statements.as_slice()
        else {
            panic!("unexpected statement layout");
        };

        let Statement::WithParams(query, _) = delete_api_release else {
            panic!("expected delete release statement");
        };
        assert_eq!(
            query,
            "DELETE FROM service_releases WHERE namespace = ? AND service = ?"
        );

        let Statement::WithParams(query, _) = delete_worker_release else {
            panic!("expected delete release statement");
        };
        assert_eq!(
            query,
            "DELETE FROM service_releases WHERE namespace = ? AND service = ?"
        );

        let Statement::WithParams(query, _) = upsert_api_release else {
            panic!("expected release upsert statement");
        };
        assert!(query.starts_with("INSERT INTO service_releases"));

        let Statement::WithParams(query, _) = upsert_worker_release else {
            panic!("expected release upsert statement");
        };
        assert!(query.starts_with("INSERT INTO service_releases"));

        let Statement::WithParams(query, _) = upsert_deploy else {
            panic!("expected deploy upsert statement");
        };
        assert!(query.starts_with("INSERT INTO deploys"));
    }
}
