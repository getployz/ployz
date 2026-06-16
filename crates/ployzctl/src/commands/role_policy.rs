use clap::Args;
use ployz_core::roles::InstallRolePolicy;

/// Shared CLI shape for optional machine roles.
///
/// The alpha default installs gateway and DNS everywhere; flags are explicit
/// opt-outs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Args)]
pub(crate) struct RolePolicyCli {
    #[arg(long)]
    no_gateway: bool,
    #[arg(long)]
    no_dns: bool,
}

impl RolePolicyCli {
    #[must_use]
    pub(crate) const fn into_policy(self) -> InstallRolePolicy {
        let mut policy = InstallRolePolicy::install_all();
        if self.no_gateway {
            policy = policy.without_gateway();
        }
        if self.no_dns {
            policy = policy.without_dns();
        }
        policy
    }

    #[must_use]
    pub(crate) const fn has_explicit_flags(self) -> bool {
        self.no_gateway || self.no_dns
    }
}
