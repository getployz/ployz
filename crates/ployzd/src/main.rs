use ployzd::app::plan_configured_process;
use ployzd::config::{LoadedDaemonProcessConfig, load_daemon_process_config};
use ployzd::role::parse_role_args;

fn main() {
    match parse_role_args(std::env::args().skip(1)) {
        Ok(role) => match load_daemon_process_config(role.clone(), env_var) {
            Ok(LoadedDaemonProcessConfig::Configured(config)) => {
                let plan = plan_configured_process(&config);
                println!("ployzd role: {}", config.role().process_name());
                println!("ployzd plan: {}", plan_name(&plan));
            }
            Ok(LoadedDaemonProcessConfig::TunnelConfigPending { .. }) => {
                println!("ployzd role: {}", role.process_name());
                println!("ployzd tunnel config: pending");
            }
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(2);
            }
        },
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(2);
        }
    }
}

fn env_var(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

fn plan_name(plan: &ployzd::app::RoleProcessPlan) -> &'static str {
    match plan {
        ployzd::app::RoleProcessPlan::Control(_) => "control",
        ployzd::app::RoleProcessPlan::Node(_) => "node",
        ployzd::app::RoleProcessPlan::Gateway(_) => "gateway",
        ployzd::app::RoleProcessPlan::Dns(_) => "dns",
        ployzd::app::RoleProcessPlan::Tunnel(_) => "tunnel",
    }
}
