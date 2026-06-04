use super::fixtures::*;

#[tokio::test]
async fn operation_repository_duplicate_submit_returns_original_operation() {
    let nats = test_nats().await;
    let repository = operation_repository(&nats.jetstream).await;

    let first = repository
        .submit_deploy(deploy_submission("op_123", "idem_1", "svc_api"))
        .await
        .expect("first submit accepted");
    let second = repository
        .submit_deploy(deploy_submission("op_456", "idem_1", "svc_other"))
        .await
        .expect("duplicate submit accepted");

    assert_eq!(first, second);
    assert_eq!(first.operation_id, operation_id("op_123"));
    assert!(
        repository
            .operation_status(&operation_id("op_456"))
            .await
            .expect("status lookup succeeds")
            .is_none()
    );
}
