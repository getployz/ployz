use ployz_sdk_types::AcceptedOperation;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetachedDeployOutput {
    pub accepted: AcceptedOperation,
}

impl DetachedDeployOutput {
    #[must_use]
    pub fn render(&self) -> String {
        format!(
            "operation {}\nwatch ployzctl ops watch {}\n",
            self.accepted.operation_id.as_str(),
            self.accepted.operation_id.as_str()
        )
    }
}
