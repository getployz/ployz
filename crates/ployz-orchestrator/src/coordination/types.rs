use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ReservationId(pub String);

impl ReservationId {
    #[must_use]
    pub fn random() -> Self {
        let mut bytes = [0u8; 16];
        rand::fill(&mut bytes);
        let mut value = String::with_capacity(32);
        for byte in bytes {
            use std::fmt::Write as _;
            let _ = write!(&mut value, "{byte:02x}");
        }
        Self(value)
    }
}
