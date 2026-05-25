//! Polis adapter helpers for Ployz composition code.

mod acme_attempt;
mod domain;
mod failure_codecs;
mod machine_membership;
mod scalars;
mod serving;

pub(crate) use acme_attempt::{
    certificate_attempt_schema_statements, start_corrosion_certificate_attempts,
    verify_certificate_attempt_schema,
};
pub(crate) use domain::{
    domain_status_schema_statements, start_corrosion_domain_status, verify_domain_status_schema,
};
pub(crate) use machine_membership::start_corrosion_machine_membership;
pub(crate) use serving::{
    serving_snapshot_schema_statements, start_corrosion_serving_snapshots,
    verify_serving_snapshot_schema,
};
