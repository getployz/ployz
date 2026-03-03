#[derive(Debug, Clone)]
pub struct PingoraAdapter {
    pub admin_url: String,
}

impl PingoraAdapter {
    pub fn new(admin_url: impl Into<String>) -> Self {
        Self {
            admin_url: admin_url.into(),
        }
    }
}
