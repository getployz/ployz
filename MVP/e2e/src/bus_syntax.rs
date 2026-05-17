use mvp_bus::{FactKey, FactKeyPattern, Subject, SubjectPattern};

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
