#[derive(Debug, Clone)]
pub struct CorrosionAdapter {
    pub endpoint: String,
}

impl CorrosionAdapter {
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
        }
    }
}
