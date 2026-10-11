use super::*;

fn store_error(code: RpcErrorCode, details: Value) -> RpcError {
    RpcError {
        code,
        message: "refused".to_owned(),
        details,
        cause: Vec::new(),
    }
}

#[test]
fn a_project_confirmation_retries_with_its_volumes_as_environment_slash_volume() {
    let call = Call::new("project_remove", json!({ "project": "shop" }));
    let error = store_error(
        RpcErrorCode::ConfirmationRequired,
        json!({
            "volumes": {}, "effects": [], "version": "v1",
            "accept": { "production": ["data"], "staging": ["cache", "data"] },
        }),
    );

    let refusal = refused(Target::Project, error, &call, Writes::None);

    assert_eq!(
        refusal.retry,
        Call::new(
            "project_remove",
            json!({
                "project": "shop",
                "accept_volume_loss": ["production/data", "staging/cache", "staging/data"],
                "version": "v1",
            }),
        )
    );
}

#[test]
fn deleting_every_copy_is_offered_in_details_and_never_the_retry() {
    let call = Call::new("environment_remove", json!({ "environment": "production" }));
    let error = store_error(
        RpcErrorCode::Unavailable,
        json!({
            "waiting_on": [],
            "every_copy": {
                "tool": "remove",
                "input": {
                    "project": "shop", "environment": "production", "version": "v2",
                    "accept_volume_loss": { "production": ["data"] },
                    "every_copy": { "production": ["data"] },
                },
            },
        }),
    );

    let refusal = refused(Target::Environment, error, &call, Writes::None);

    assert!(
        refusal.retry.input.get("every_copy").is_none(),
        "{refusal:?}"
    );
    assert_eq!(
        refusal.details.get("every_copy"),
        Some(&json!({
            "tool": "environment_remove",
            "input": {
                "environment": "production",
                "accept_volume_loss": ["data"],
                "every_copy": ["data"],
                "version": "v2",
            },
        }))
    );
}

#[test]
fn a_volume_named_without_its_environment_is_refused() {
    let Err(Stop::Refuse(refusal)) = by_environment(&["data".to_owned()], "every_copy") else {
        panic!("a bare Volume name is refused");
    };
    assert_eq!(refusal.field.as_deref(), Some("every_copy"));
}
