use mvp_bus::{BusSession, FactKey, FactKeyPattern, Subject, SubjectPattern, harness::InMemoryBus};
use mvp_projection::ProjectionFactPayload;

pub(crate) fn subject(value: &str) -> Result<Subject, String> {
    Subject::parse(value).map_err(|error| error.to_string())
}

pub(crate) fn pattern(value: &str) -> Result<SubjectPattern, String> {
    SubjectPattern::parse(value).map_err(|error| error.to_string())
}

pub(crate) fn fact_key(value: &str) -> Result<FactKey, String> {
    FactKey::parse(value).map_err(|error| error.to_string())
}

pub(crate) fn fact_pattern(value: &str) -> Result<FactKeyPattern, String> {
    FactKeyPattern::parse(value).map_err(|error| error.to_string())
}

pub(crate) fn write_projection_fact(
    bus: &InMemoryBus,
    session: &BusSession,
    key: &str,
    payload: ProjectionFactPayload,
) -> Result<(), String> {
    bus.write_fact_payload(
        session,
        fact_key(key)?,
        payload
            .to_fact_bytes()
            .map_err(|error| format!("serialize projection fact '{key}': {error}"))?,
    )
    .map(|_| ())
    .map_err(|error| format!("write projection fact '{key}': {error}"))
}
