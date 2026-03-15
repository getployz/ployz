use crate::client::CorrClient;
use crate::store::shared::sql::exec_all;
use crate::store::tables::{deploys, service_heads, service_slots};
use corro_api_types::Statement;
use ployz_sdk::error::Result;
use ployz_sdk::model::{DeployRecord, ServiceHeadRecord, ServiceSlotRecord};
use ployz_sdk::spec::Namespace;

pub(crate) async fn commit_deploy(
    client: &CorrClient,
    namespace: &Namespace,
    removed_services: &[String],
    heads: &[ServiceHeadRecord],
    slots: &[ServiceSlotRecord],
    deploy: &DeployRecord,
) -> Result<()> {
    let statements = build_commit_statements(namespace, removed_services, heads, slots, deploy);
    exec_all(client, &statements, "commit_deploy").await
}

fn touched_services(removed_services: &[String], heads: &[ServiceHeadRecord]) -> Vec<String> {
    let mut touched = removed_services.to_vec();
    for head in heads {
        if !touched.contains(&head.service) {
            touched.push(head.service.clone());
        }
    }
    touched
}

fn build_commit_statements(
    namespace: &Namespace,
    removed_services: &[String],
    heads: &[ServiceHeadRecord],
    slots: &[ServiceSlotRecord],
    deploy: &DeployRecord,
) -> Vec<Statement> {
    let touched = touched_services(removed_services, heads);
    let mut statements = Vec::new();

    for service in &touched {
        statements.push(service_heads::delete_statement(namespace, service));
        statements.push(service_slots::delete_statement(namespace, service));
    }

    for head in heads {
        statements.push(service_heads::insert_statement(head));
    }

    for slot in slots {
        statements.push(service_slots::insert_statement(slot));
    }

    statements.push(deploys::upsert_statement(deploy));

    statements
}

#[cfg(test)]
mod tests {
    use super::build_commit_statements;
    use corro_api_types::Statement;
    use ployz_sdk::model::{
        DeployId, DeployRecord, DeployState, InstanceId, MachineId, ServiceHeadRecord,
        ServiceSlotRecord, SlotId,
    };
    use ployz_sdk::spec::Namespace;

    #[test]
    fn build_commit_statements_deduplicates_services_and_preserves_order() {
        let namespace = Namespace(String::from("ns"));
        let removed_services = vec![String::from("api")];
        let heads = vec![
            ServiceHeadRecord {
                namespace: namespace.clone(),
                service: String::from("api"),
                current_revision_hash: String::from("rev-api"),
                updated_by_deploy_id: DeployId(String::from("deploy-1")),
                updated_at: 10,
            },
            ServiceHeadRecord {
                namespace: namespace.clone(),
                service: String::from("worker"),
                current_revision_hash: String::from("rev-worker"),
                updated_by_deploy_id: DeployId(String::from("deploy-1")),
                updated_at: 11,
            },
        ];
        let slots = vec![ServiceSlotRecord {
            namespace: namespace.clone(),
            service: String::from("worker"),
            slot_id: SlotId(String::from("slot-1")),
            machine_id: MachineId(String::from("machine-1")),
            active_instance_id: InstanceId(String::from("instance-1")),
            revision_hash: String::from("rev-worker"),
            updated_by_deploy_id: DeployId(String::from("deploy-1")),
            updated_at: 12,
        }];
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

        let statements =
            build_commit_statements(&namespace, &removed_services, &heads, &slots, &deploy);

        let [
            delete_api_head,
            delete_api_slot,
            delete_worker_head,
            delete_worker_slot,
            insert_api_head,
            insert_worker_head,
            insert_worker_slot,
            upsert_deploy,
        ] = statements.as_slice()
        else {
            panic!("unexpected statement layout");
        };

        let Statement::WithParams(query, _) = delete_api_head else {
            panic!("expected delete head statement");
        };
        assert_eq!(
            query,
            "DELETE FROM service_heads WHERE namespace = ? AND service = ?"
        );

        let Statement::WithParams(query, _) = delete_api_slot else {
            panic!("expected delete slot statement");
        };
        assert_eq!(
            query,
            "DELETE FROM service_slots WHERE namespace = ? AND service = ?"
        );

        let Statement::WithParams(query, params) = delete_worker_head else {
            panic!("expected delete head statement");
        };
        assert_eq!(
            query,
            "DELETE FROM service_heads WHERE namespace = ? AND service = ?"
        );
        assert_eq!(params.len(), 2);

        let Statement::WithParams(query, params) = delete_worker_slot else {
            panic!("expected delete slot statement");
        };
        assert_eq!(
            query,
            "DELETE FROM service_slots WHERE namespace = ? AND service = ?"
        );
        assert_eq!(params.len(), 2);

        let Statement::WithParams(query, _) = insert_api_head else {
            panic!("expected head insert statement");
        };
        assert_eq!(
            query,
            "INSERT INTO service_heads (namespace, service, current_revision_hash, updated_by_deploy_id, updated_at) VALUES (?, ?, ?, ?, ?)"
        );

        let Statement::WithParams(query, _) = insert_worker_head else {
            panic!("expected head insert statement");
        };
        assert_eq!(
            query,
            "INSERT INTO service_heads (namespace, service, current_revision_hash, updated_by_deploy_id, updated_at) VALUES (?, ?, ?, ?, ?)"
        );

        let Statement::WithParams(query, _) = insert_worker_slot else {
            panic!("expected slot insert statement");
        };
        assert_eq!(
            query,
            "INSERT INTO service_slots (namespace, service, slot_id, machine_id, active_instance_id, revision_hash, updated_by_deploy_id, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)"
        );

        let Statement::WithParams(query, _) = upsert_deploy else {
            panic!("expected deploy upsert statement");
        };
        assert!(query.starts_with("INSERT INTO deploys"));
    }
}
