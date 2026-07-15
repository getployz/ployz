//! Control startup, durable intent, authorization, projections, and recovery journeys.

mod support;

#[path = "control/certificate.rs"]
mod certificate;
#[path = "control/intent.rs"]
mod intent;
#[path = "control/machine_add.rs"]
mod machine_add;
#[path = "control/managed_dns.rs"]
mod managed_dns;
#[path = "control/nats_bootstrap.rs"]
mod nats_bootstrap;
#[path = "control/runtime.rs"]
mod runtime;
#[path = "control/wildcard_certificate.rs"]
mod wildcard_certificate;
