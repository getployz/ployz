//! Every compiled command: its place in the tree, its handler and what agents need
//! to know before calling it. `VERBS` is the only list.

use clap::Command;
use serde::Serialize;

#[cfg(feature = "verify-faults")]
use super::debug;
use super::{
    Handler, account, build, catalog, cloud, completion, config, config_item, context, deploy,
    docs, domain, env, github, link, login, mcp, metrics, operator, project, review, server,
    service, up, volume,
};
use Approval::{Always, Depends, Never};

/// Whether a call can destroy something live, and so whether it needs approval.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Approval {
    /// A read, or an edit that only stages Working State.
    Never,
    /// Destroys a live thing at once.
    Always,
    /// Destroys whatever the plan it approves destroys.
    Depends,
}

/// Where a command can run: against Cloud, only on this device, or only inside Ployz.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Surface {
    Cloud,
    Local,
    Internal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Stdin {
    Only,
    OnDash,
    IfAbsent,
}

/// Whether a command prints a `--json` result. Refusers own a terminal session,
/// print shell code, or print the Cloud runner's own fixed JSON.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Json {
    Result,
    Refused,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum KeepsRunning {
    Never,
    Always,
    /// Only when this argument is given.
    With(&'static str),
}

#[derive(Clone, Copy)]
pub(crate) struct Verb {
    pub(crate) path: &'static str,
    pub(crate) command: fn(Command) -> Command,
    pub(crate) run: Handler,
    pub(crate) approval: Approval,
    pub(crate) surface: Surface,
    pub(crate) json: Json,
    pub(crate) stdin: &'static [(&'static str, Stdin)],
    pub(crate) keeps_running: KeepsRunning,
}

impl Verb {
    const fn new(
        path: &'static str,
        surface: Surface,
        approval: Approval,
        command: fn(Command) -> Command,
        run: Handler,
    ) -> Self {
        Self {
            path,
            command,
            run,
            approval,
            surface,
            json: Json::Result,
            stdin: &[],
            keeps_running: KeepsRunning::Never,
        }
    }

    const fn cloud(
        path: &'static str,
        approval: Approval,
        command: fn(Command) -> Command,
        run: Handler,
    ) -> Self {
        Self::new(path, Surface::Cloud, approval, command, run)
    }

    const fn local(
        path: &'static str,
        approval: Approval,
        command: fn(Command) -> Command,
        run: Handler,
    ) -> Self {
        Self::new(path, Surface::Local, approval, command, run)
    }

    const fn internal(
        path: &'static str,
        approval: Approval,
        command: fn(Command) -> Command,
        run: Handler,
    ) -> Self {
        Self::new(path, Surface::Internal, approval, command, run)
    }

    const fn refuses_json(self) -> Self {
        Self {
            json: Json::Refused,
            ..self
        }
    }

    const fn stdin(self, stdin: &'static [(&'static str, Stdin)]) -> Self {
        Self { stdin, ..self }
    }

    const fn keeps_running(self, keeps_running: KeepsRunning) -> Self {
        Self {
            keeps_running,
            ..self
        }
    }
}

/// A parent with no handler of its own.
struct Group {
    path: &'static str,
    command: fn(Command) -> Command,
}

pub(crate) const BUILD: Verb =
    Verb::internal("build", Never, build::command, build::build).refuses_json();
pub(crate) const CLOUD_RESET: Verb =
    Verb::cloud("cloud reset", Always, cloud::reset_command, cloud::reset);
pub(crate) const COMPLETION: Verb =
    Verb::local("completion", Never, crate::cli::completion, completion).refuses_json();
pub(crate) const CONFIG_ADD: Verb = Verb::cloud(
    "config add",
    Never,
    config_item::create_command,
    config_item::create,
);
pub(crate) const CONFIG_INSPECT: Verb = Verb::cloud(
    "config inspect",
    Never,
    config_item::inspect_command,
    config_item::inspect,
);
pub(crate) const CONFIG_LS: Verb = Verb::cloud(
    "config ls",
    Never,
    config_item::list_command,
    config_item::list,
);
pub(crate) const CONFIG_MOUNT: Verb = Verb::cloud(
    "config mount",
    Never,
    config_item::mount_command,
    config_item::mount,
);
pub(crate) const CONFIG_PUT: Verb = Verb::cloud(
    "config put",
    Never,
    config_item::put_command,
    config_item::put,
)
.stdin(&[("--from", Stdin::IfAbsent)]);
pub(crate) const CONFIG_RENAME: Verb = Verb::cloud(
    "config rename",
    Never,
    config_item::rename_command,
    config_item::rename,
);
pub(crate) const CONFIG_RM: Verb = Verb::cloud(
    "config rm",
    Never,
    config_item::delete_command,
    config_item::delete,
);
pub(crate) const CONFIG_RM_FILE: Verb = Verb::cloud(
    "config rm-file",
    Never,
    config_item::remove_file_command,
    config_item::remove_file,
);
pub(crate) const CONFIG_UNMOUNT: Verb = Verb::cloud(
    "config unmount",
    Never,
    config_item::unmount_command,
    config_item::unmount,
);
pub(crate) const CTX: Verb = Verb::local("ctx", Never, context::command, link::show);
pub(crate) const CTX_LS: Verb = Verb::local("ctx ls", Never, context::list_command, context::list);
pub(crate) const CTX_RM: Verb =
    Verb::local("ctx rm", Never, context::remove_command, context::remove);
pub(crate) const CTX_USE: Verb =
    Verb::local("ctx use", Never, context::select_command, context::select);
#[cfg(feature = "verify-faults")]
pub(crate) const DEBUG_LOCAL_RPC: Verb = Verb::internal(
    "debug local-rpc",
    Never,
    debug::local_rpc_command,
    debug::local_rpc,
);
#[cfg(feature = "verify-faults")]
pub(crate) const DEBUG_VOLUME_RPC: Verb = Verb::internal(
    "debug volume-rpc",
    Never,
    debug::volume_rpc_command,
    debug::volume_rpc,
);
pub(crate) const DEPLOY: Verb =
    Verb::cloud("deploy", Depends, deploy::deploy_command, deploy::deploy);
pub(crate) const DEPLOYMENT_CANCEL: Verb = Verb::cloud(
    "deployment cancel",
    Never,
    deploy::cancel_command,
    deploy::cancel,
);
pub(crate) const DEPLOYMENT_LS: Verb =
    Verb::cloud("deployment ls", Never, deploy::ls_command, deploy::ls);
pub(crate) const DEPLOYMENT_RETRY: Verb = Verb::cloud(
    "deployment retry",
    Never,
    deploy::retry_command,
    deploy::retry,
);
pub(crate) const DEPLOYMENT_SHOW: Verb =
    Verb::cloud("deployment show", Never, deploy::show_command, deploy::show);
pub(crate) const DEPLOYMENT_START: Verb = Verb::cloud(
    "deployment start",
    Never,
    deploy::start_command,
    deploy::start,
);
pub(crate) const DIFF: Verb = Verb::cloud("diff", Never, review::diff_command, review::diff);
pub(crate) const DISCARD: Verb =
    Verb::cloud("discard", Never, review::discard_command, review::discard);
pub(crate) const DOCS: Verb = Verb::local("docs", Never, docs::command, docs::run);
pub(crate) const DOMAIN_ADD: Verb =
    Verb::cloud("domain add", Never, domain::add_command, domain::add);
pub(crate) const DOMAIN_CHECK: Verb =
    Verb::cloud("domain check", Never, domain::check_command, domain::check);
pub(crate) const DOMAIN_LS: Verb =
    Verb::cloud("domain ls", Never, domain::list_command, domain::list);
pub(crate) const DOMAIN_RM: Verb =
    Verb::cloud("domain rm", Never, domain::remove_command, domain::remove);
pub(crate) const DOMAIN_SET: Verb =
    Verb::cloud("domain set", Never, domain::set_command, domain::set);
pub(crate) const ENV_BRANCH: Verb = Verb::cloud(
    "env branch",
    Never,
    env::branch_command,
    env::branch::branch,
);
pub(crate) const ENV_COPY: Verb =
    Verb::cloud("env copy", Never, env::copy_command, env::branch::copy);
pub(crate) const ENV_DEFAULT: Verb =
    Verb::cloud("env default", Never, env::default_command, env::default);
pub(crate) const ENV_KEEP: Verb =
    Verb::cloud("env keep", Never, env::keep_command, env::branch::keep);
pub(crate) const ENV_LS: Verb = Verb::cloud("env ls", Never, env::ls_command, env::ls);
pub(crate) const ENV_NEVER_SYNC: Verb = Verb::cloud(
    "env never-sync",
    Never,
    env::never_sync_command,
    env::never_sync,
);
pub(crate) const ENV_NEW: Verb = Verb::cloud("env new", Never, env::new_command, env::new);
pub(crate) const ENV_ON_MERGE: Verb = Verb::cloud(
    "env on-merge",
    Never,
    env::on_merge_command,
    env::pr::on_merge,
);
pub(crate) const ENV_PR: Verb = Verb::cloud("env pr", Depends, env::pr_command, env::pr::pr);
pub(crate) const ENV_RM: Verb = Verb::cloud("env rm", Always, env::rm_command, env::rm);
pub(crate) const ENV_SETUP: Verb = Verb::cloud("env setup", Never, env::setup_command, env::setup);
pub(crate) const ENV_SHUTDOWN: Verb = Verb::cloud(
    "env shutdown",
    Always,
    env::shutdown_command,
    env::pr::shutdown,
);
pub(crate) const ENV_SYNC: Verb =
    Verb::cloud("env sync", Depends, env::sync_command, env::branch::sync)
        .stdin(&[("--value", Stdin::Only)]);
pub(crate) const EXEC: Verb =
    Verb::cloud("exec", Always, operator::exec_command, operator::exec).refuses_json();
pub(crate) const EXPLAIN: Verb =
    Verb::cloud("explain", Never, catalog::explain_command, catalog::explain);
pub(crate) const GET: Verb = Verb::cloud("get", Never, config::get_command, config::get);
pub(crate) const GITHUB_CAT: Verb =
    Verb::cloud("github cat", Never, github::cat_command, github::cat);
pub(crate) const GITHUB_CONNECT: Verb = Verb::cloud(
    "github connect",
    Never,
    github::connect_command,
    github::connect,
)
.keeps_running(KeepsRunning::With("--wait"));
pub(crate) const GITHUB_DISCONNECT: Verb = Verb::cloud(
    "github disconnect",
    Always,
    github::disconnect_command,
    github::disconnect,
);
pub(crate) const GITHUB_LS: Verb =
    Verb::cloud("github ls", Never, github::list_command, github::list);
pub(crate) const GITHUB_TREE: Verb =
    Verb::cloud("github tree", Never, github::tree_command, github::tree);
pub(crate) const HISTORY: Verb =
    Verb::cloud("history", Never, review::history_command, review::history);
pub(crate) const HISTORY_RESTORE: Verb = Verb::cloud(
    "history restore",
    Never,
    review::restore_command,
    review::history,
);
pub(crate) const HISTORY_UNDO: Verb =
    Verb::cloud("history undo", Never, review::undo_command, review::history);
pub(crate) const LINK: Verb = Verb::local("link", Never, link::link_command, link::link);
pub(crate) const LOGIN: Verb = Verb::local("login", Never, login::login_command, login::login);
pub(crate) const LOGOUT: Verb = Verb::local("logout", Never, login::logout_command, login::logout);
pub(crate) const LOGS: Verb = Verb::cloud("logs", Never, operator::logs_command, operator::logs)
    .keeps_running(KeepsRunning::With("--follow"));
pub(crate) const MCP: Verb = Verb::local("mcp", Never, mcp::command, mcp::serve).refuses_json();
pub(crate) const METRICS: Verb = Verb::cloud("metrics", Never, metrics::command, metrics::metrics);
pub(crate) const ORG_BUILD_ORDER: Verb = Verb::cloud(
    "org build-order",
    Depends,
    account::org_build_order_command,
    account::org_build_order,
);
pub(crate) const ORG_LEAVE: Verb = Verb::cloud(
    "org leave",
    Always,
    account::org_leave_command,
    account::org_leave,
);
pub(crate) const ORG_LS: Verb = Verb::cloud(
    "org ls",
    Never,
    account::org_list_command,
    account::org_list,
);
pub(crate) const ORG_RM: Verb = Verb::cloud(
    "org rm",
    Always,
    account::org_remove_command,
    account::org_remove,
);
pub(crate) const ORG_USE: Verb =
    Verb::cloud("org use", Never, account::org_use_command, account::org_use);
pub(crate) const PROJECT_LS: Verb =
    Verb::cloud("project ls", Never, project::ls_command, project::ls);
pub(crate) const PROJECT_NEW: Verb =
    Verb::cloud("project new", Never, project::new_command, project::new);
pub(crate) const PROJECT_RENAME: Verb = Verb::cloud(
    "project rename",
    Never,
    project::rename_command,
    project::rename,
);
pub(crate) const PROJECT_RM: Verb =
    Verb::cloud("project rm", Always, project::rm_command, project::rm);
pub(crate) const PS: Verb = Verb::cloud("ps", Never, operator::ps_command, service::processes);
pub(crate) const SCHEMA: Verb =
    Verb::cloud("schema", Never, catalog::schema_command, catalog::schema);
pub(crate) const SERVER_ADD: Verb = Verb::cloud(
    "server add",
    Depends,
    server::enroll::command,
    server::enroll::add,
)
.keeps_running(KeepsRunning::With("--wait"));
pub(crate) const SERVER_BUILD_CACHE_CLEAR: Verb = Verb::local(
    "server build-cache-clear",
    Never,
    server::clear_build_cache_command,
    server::clear_build_cache,
);
pub(crate) const SERVER_CLEAN: Verb = Verb::cloud(
    "server clean",
    Depends,
    server::clean::command,
    server::clean::clean,
);
pub(crate) const SERVER_DRAIN: Verb = Verb::cloud(
    "server drain",
    Depends,
    server::drain_command,
    server::drain::drain,
);
pub(crate) const SERVER_FORGET: Verb = Verb::cloud(
    "server forget",
    Always,
    server::forget::command,
    server::forget::forget,
);
pub(crate) const SERVER_INSPECT: Verb = Verb::cloud(
    "server inspect",
    Never,
    server::inspect_command,
    server::inspect,
);
pub(crate) const SERVER_LOGS: Verb = Verb::cloud(
    "server logs",
    Never,
    server::machine_logs_command,
    operator::machine_logs,
)
.keeps_running(KeepsRunning::With("--follow"));
pub(crate) const SERVER_LS: Verb =
    Verb::cloud("server ls", Never, server::list_command, server::list);
pub(crate) const SERVER_RM: Verb =
    Verb::cloud("server rm", Always, server::remove_command, server::remove);
pub(crate) const SERVER_SET: Verb =
    Verb::cloud("server set", Depends, server::set_command, server::set);
pub(crate) const SERVER_UPGRADE: Verb = Verb::cloud(
    "server upgrade",
    Depends,
    server::upgrade_command,
    server::upgrade,
);
pub(crate) const SERVICE_ADD: Verb = Verb::cloud(
    "service add",
    Never,
    service::authored::add_command,
    service::authored::add,
);
pub(crate) const SERVICE_INSPECT: Verb = Verb::cloud(
    "service inspect",
    Never,
    service::authored::inspect_command,
    service::authored::inspect,
);
pub(crate) const SERVICE_LS: Verb = Verb::cloud(
    "service ls",
    Never,
    service::authored::list_command,
    service::authored::list,
);
pub(crate) const SERVICE_PORT_FORWARD: Verb = Verb::cloud(
    "service port-forward",
    Never,
    service::port_forward_command,
    operator::port_forward,
)
.keeps_running(KeepsRunning::Always);
pub(crate) const SERVICE_RENAME: Verb = Verb::cloud(
    "service rename",
    Never,
    service::authored::rename_command,
    service::authored::rename,
);
pub(crate) const SERVICE_RESTART: Verb = Verb::cloud(
    "service restart",
    Never,
    service::restart_command,
    service::restart,
);
pub(crate) const SERVICE_RM: Verb = Verb::cloud(
    "service rm",
    Never,
    service::authored::remove_command,
    service::authored::remove,
);
pub(crate) const SERVICE_START: Verb = Verb::cloud(
    "service start",
    Never,
    service::start_command,
    service::start,
);
pub(crate) const SERVICE_STOP: Verb =
    Verb::cloud("service stop", Never, service::stop_command, service::stop);
pub(crate) const SET: Verb = Verb::cloud("set", Never, config::set_command, config::set).stdin(&[
    ("--from-env-file", Stdin::OnDash),
    ("--patch", Stdin::OnDash),
    ("--secret", Stdin::Only),
]);
pub(crate) const STATUS: Verb = Verb::cloud("status", Never, link::status_command, link::status);
pub(crate) const TOKEN_LS: Verb = Verb::cloud(
    "token ls",
    Never,
    account::token_list_command,
    account::token_list,
);
pub(crate) const TOKEN_NEW: Verb = Verb::cloud(
    "token new",
    Always,
    account::token_new_command,
    account::token_new,
);
pub(crate) const TOKEN_RM: Verb = Verb::cloud(
    "token rm",
    Always,
    account::token_remove_command,
    account::token_remove,
);
pub(crate) const UNSET: Verb = Verb::cloud("unset", Never, config::unset_command, config::unset);
pub(crate) const UP: Verb = Verb::cloud("up", Depends, up::command, up::up);
pub(crate) const VOLUME_ADD: Verb =
    Verb::cloud("volume add", Never, volume::add_command, volume::add);
pub(crate) const VOLUME_INSPECT: Verb = Verb::cloud(
    "volume inspect",
    Never,
    volume::inspect_command,
    volume::inspect,
);
pub(crate) const VOLUME_LS: Verb =
    Verb::cloud("volume ls", Never, volume::list_command, volume::list);
pub(crate) const VOLUME_MIRROR: Verb = Verb::cloud(
    "volume mirror",
    Never,
    volume::run::mirror_command,
    volume::run::mirror,
)
.keeps_running(KeepsRunning::With("--wait"));
pub(crate) const VOLUME_MIRROR_RM: Verb = Verb::cloud(
    "volume mirror rm",
    Always,
    volume::run::remove_mirror_command,
    volume::run::remove_mirror,
)
.keeps_running(KeepsRunning::With("--wait"));
pub(crate) const VOLUME_MOVE: Verb = Verb::cloud(
    "volume move",
    Never,
    volume::run::move_command,
    volume::run::move_volume,
)
.keeps_running(KeepsRunning::With("--wait"));
pub(crate) const VOLUME_RELEASE: Verb = Verb::cloud(
    "volume release",
    Never,
    volume::run::release_command,
    volume::run::release,
)
.keeps_running(KeepsRunning::With("--wait"));
pub(crate) const VOLUME_RENAME: Verb = Verb::cloud(
    "volume rename",
    Never,
    volume::rename_command,
    volume::rename,
);
pub(crate) const VOLUME_RESTORE: Verb = Verb::cloud(
    "volume restore",
    Always,
    volume::run::restore_command,
    volume::run::restore,
)
.keeps_running(KeepsRunning::With("--wait"));
pub(crate) const VOLUME_RM: Verb =
    Verb::cloud("volume rm", Never, volume::remove_command, volume::remove);
pub(crate) const VOLUME_RUNS: Verb = Verb::cloud(
    "volume runs",
    Never,
    volume::run::runs_command,
    volume::run::runs,
);
pub(crate) const VOLUME_SET: Verb =
    Verb::cloud("volume set", Never, volume::set_command, volume::set);
pub(crate) const VOLUME_SYNC: Verb = Verb::cloud(
    "volume sync",
    Never,
    volume::run::sync_command,
    volume::run::sync,
)
.keeps_running(KeepsRunning::With("--wait"));

pub(crate) const VERBS: &[Verb] = &[
    BUILD,
    CLOUD_RESET,
    COMPLETION,
    CONFIG_ADD,
    CONFIG_INSPECT,
    CONFIG_LS,
    CONFIG_MOUNT,
    CONFIG_PUT,
    CONFIG_RENAME,
    CONFIG_RM,
    CONFIG_RM_FILE,
    CONFIG_UNMOUNT,
    CTX,
    CTX_LS,
    CTX_RM,
    CTX_USE,
    #[cfg(feature = "verify-faults")]
    DEBUG_LOCAL_RPC,
    #[cfg(feature = "verify-faults")]
    DEBUG_VOLUME_RPC,
    DEPLOY,
    DEPLOYMENT_CANCEL,
    DEPLOYMENT_LS,
    DEPLOYMENT_RETRY,
    DEPLOYMENT_SHOW,
    DEPLOYMENT_START,
    DIFF,
    DISCARD,
    DOCS,
    DOMAIN_ADD,
    DOMAIN_CHECK,
    DOMAIN_LS,
    DOMAIN_RM,
    DOMAIN_SET,
    ENV_BRANCH,
    ENV_COPY,
    ENV_DEFAULT,
    ENV_KEEP,
    ENV_LS,
    ENV_NEVER_SYNC,
    ENV_NEW,
    ENV_ON_MERGE,
    ENV_PR,
    ENV_RM,
    ENV_SETUP,
    ENV_SHUTDOWN,
    ENV_SYNC,
    EXEC,
    EXPLAIN,
    GET,
    GITHUB_CAT,
    GITHUB_CONNECT,
    GITHUB_DISCONNECT,
    GITHUB_LS,
    GITHUB_TREE,
    HISTORY,
    HISTORY_RESTORE,
    HISTORY_UNDO,
    LINK,
    LOGIN,
    LOGOUT,
    LOGS,
    MCP,
    METRICS,
    ORG_BUILD_ORDER,
    ORG_LEAVE,
    ORG_LS,
    ORG_RM,
    ORG_USE,
    PROJECT_LS,
    PROJECT_NEW,
    PROJECT_RENAME,
    PROJECT_RM,
    PS,
    SCHEMA,
    SERVER_ADD,
    SERVER_BUILD_CACHE_CLEAR,
    SERVER_CLEAN,
    SERVER_DRAIN,
    SERVER_FORGET,
    SERVER_INSPECT,
    SERVER_LOGS,
    SERVER_LS,
    SERVER_RM,
    SERVER_SET,
    SERVER_UPGRADE,
    SERVICE_ADD,
    SERVICE_INSPECT,
    SERVICE_LS,
    SERVICE_PORT_FORWARD,
    SERVICE_RENAME,
    SERVICE_RESTART,
    SERVICE_RM,
    SERVICE_START,
    SERVICE_STOP,
    SET,
    STATUS,
    TOKEN_LS,
    TOKEN_NEW,
    TOKEN_RM,
    UNSET,
    UP,
    VOLUME_ADD,
    VOLUME_INSPECT,
    VOLUME_LS,
    VOLUME_MIRROR,
    VOLUME_MIRROR_RM,
    VOLUME_MOVE,
    VOLUME_RELEASE,
    VOLUME_RENAME,
    VOLUME_RESTORE,
    VOLUME_RM,
    VOLUME_RUNS,
    VOLUME_SET,
    VOLUME_SYNC,
];

const GROUPS: &[Group] = &[
    Group {
        path: "cloud",
        command: cloud::group,
    },
    Group {
        path: "config",
        command: config_item::group,
    },
    #[cfg(feature = "verify-faults")]
    Group {
        path: "debug",
        command: debug::group,
    },
    Group {
        path: "deployment",
        command: deploy::deployment_group,
    },
    Group {
        path: "domain",
        command: domain::group,
    },
    Group {
        path: "env",
        command: env::group,
    },
    Group {
        path: "github",
        command: github::group,
    },
    Group {
        path: "org",
        command: account::org_group,
    },
    Group {
        path: "project",
        command: project::group,
    },
    Group {
        path: "server",
        command: server::group,
    },
    Group {
        path: "service",
        command: service::group,
    },
    Group {
        path: "token",
        command: account::token_group,
    },
    Group {
        path: "volume",
        command: volume::group,
    },
];

pub(crate) fn find(path: &str) -> Option<&'static Verb> {
    VERBS.iter().find(|verb| verb.path == path)
}

/// `root` with every group, then every verb, each under its parent.
pub(crate) fn tree(root: Command) -> Command {
    let groups = GROUPS.iter().map(|group| (group.path, group.command));
    let verbs = VERBS.iter().map(|verb| (verb.path, verb.command));
    groups.chain(verbs).fold(root, |root, (path, command)| {
        insert(root, path, path, command)
    })
}

fn insert(
    parent: Command,
    path: &'static str,
    rest: &'static str,
    command: fn(Command) -> Command,
) -> Command {
    match rest.split_once(' ') {
        None => parent.subcommand(command(Command::new(rest))),
        Some((head, rest)) => {
            assert!(
                parent.find_subcommand(head).is_some(),
                "`{path}` needs its parent `{head}` listed first"
            );
            parent.mut_subcommand(head, |child| insert(child, path, rest, command))
        }
    }
}
