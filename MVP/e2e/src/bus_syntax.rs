use mvp_bus::{Subject, SubjectPattern};

pub(crate) fn subject(value: &str) -> Result<Subject, String> {
    Subject::parse(value).map_err(|error| error.to_string())
}

pub(crate) fn pattern(value: &str) -> Result<SubjectPattern, String> {
    SubjectPattern::parse(value).map_err(|error| error.to_string())
}
