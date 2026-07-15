//! Compatibility exports for Control-owned durable operator intent.

pub mod certificate_intent {
    pub use crate::control::intent::certificate_intent::*;
}
pub mod ingress_intent {
    pub use crate::control::intent::ingress_intent::*;
}
pub mod machine_roster {
    pub use crate::control::intent::machine_roster::*;
}
pub mod namespace_intent {
    pub use crate::control::intent::namespace_intent::*;
}
pub mod nats_authorizations {
    pub use crate::control::intent::nats_authorizations::*;
}
pub mod service {
    pub use crate::control::intent::service::*;
}
