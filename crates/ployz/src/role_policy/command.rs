use clap::Args;
use ployz_core::roles::InstallRolePolicy;

/// Shared CLI shape for the optional gateway role.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Args)]
pub(crate) struct RolePolicyCli {
    #[arg(long)]
    no_gateway: bool,
}

impl RolePolicyCli {
    #[must_use]
    pub(crate) const fn into_policy(self) -> InstallRolePolicy {
        let mut policy = InstallRolePolicy::install_all();
        if self.no_gateway {
            policy = policy.without_gateway();
        }
        policy
    }

    #[must_use]
    pub(crate) const fn has_explicit_flags(self) -> bool {
        self.no_gateway
    }
}
