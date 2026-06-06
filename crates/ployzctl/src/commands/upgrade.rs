use ployz_sdk_types::AcceptedOperation;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpgradeOutput {
    pub component: UpgradeComponent,
    pub version: String,
    pub accepted: AcceptedOperation,
}

impl UpgradeOutput {
    #[must_use]
    pub fn render(&self) -> String {
        format!(
            "operation {}\nupgrade {} {}\nwatch ployzctl ops watch {}\n",
            self.accepted.operation_id.as_str(),
            self.component.as_str(),
            self.version,
            self.accepted.operation_id.as_str()
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpgradeComponent {
    Ployzd,
}

impl UpgradeComponent {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ployzd => "ployzd",
        }
    }
}
