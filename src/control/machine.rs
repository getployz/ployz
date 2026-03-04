use crate::domain::identity::Identity;

pub struct Machine {
    pub identity: Identity,
}

impl Machine {
    pub fn new(identity: Identity) -> Self {
        Self { identity }
    }
}
