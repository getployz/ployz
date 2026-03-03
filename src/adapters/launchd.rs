#[derive(Debug, Clone)]
pub struct LaunchdService {
    pub label: String,
}

impl LaunchdService {
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
        }
    }
}
