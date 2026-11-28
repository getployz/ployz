//! What one `ployz server add` asks to enroll, read once from the command line.
//!
//! Every flag that shapes the Server is read here, once, with the text it was given as, so the
//! line `--command` prints repeats exactly the flags the command line or the environment gave
//! and the engine reads the same values. A malformed value is refused when the engine reads it,
//! in the order the engine reads it, so building a plan from the command line never fails.

use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::ArgMatches;
use clap::parser::ValueSource;
use ipnet::Ipv4Net;
use ployz_core::{ClusterName, InspectRequest, MachineName, MachineRelease, StoragePreparation};

use crate::cli::{Rerun, leaf_matches, string_values};
use crate::connect::Client;
use crate::failure::Failure as Error;
use crate::provisioning::RemoteTarget;

/// The flags of `server add` that shape the Server.
pub(crate) struct Shaping {
    name: Option<String>,
    storage: Option<StoragePreparation>,
    public_ip: Option<String>,
    wg_endpoints: Vec<String>,
    wg_mtu: Option<u32>,
    labels: Vec<String>,
    region: Option<String>,
    accepts: [Option<bool>; 3],
    network: Option<Ipv4Net>,
    cluster_name: Option<String>,
    version: Option<MachineRelease>,
    reset: bool,
    yes: bool,
    ingress_closed: bool,
    pasted: Vec<String>,
}

/// Reads each flag once and notes, in the order it is read, the text a pasted line repeats
/// for it: values first, then the switches.
struct Capture<'a> {
    matches: &'a ArgMatches,
    values: Vec<String>,
    switches: Vec<String>,
}

impl Capture<'_> {
    fn given(&self, id: &str) -> bool {
        self.matches.try_contains_id(id).is_ok()
            && matches!(
                self.matches.value_source(id),
                Some(ValueSource::CommandLine | ValueSource::EnvVariable)
            )
    }

    fn say(&mut self, id: &str) {
        if self.given(id) {
            for raw in self.matches.get_raw(id).into_iter().flatten() {
                self.values.push(format!("--{id}"));
                self.values.push(raw.to_string_lossy().into_owned());
            }
        }
    }

    fn one<T: Clone + Send + Sync + 'static>(&mut self, id: &str) -> Option<T> {
        self.say(id);
        self.matches.try_get_one::<T>(id).ok().flatten().cloned()
    }

    fn many(&mut self, id: &str) -> Vec<String> {
        self.say(id);
        string_values(self.matches, id)
    }

    fn switch(&mut self, id: &str) -> bool {
        let on = self
            .matches
            .try_get_one::<bool>(id)
            .ok()
            .flatten()
            .copied()
            .unwrap_or(false);
        if on && self.given(id) {
            self.switches.push(format!("--{id}"));
        }
        on
    }
}

impl Shaping {
    pub(crate) fn from_matches(matches: &ArgMatches) -> Self {
        let mut read = Capture {
            matches,
            values: Vec::new(),
            switches: Vec::new(),
        };
        let name = read.one("name");
        let storage = read.one("storage");
        let public_ip = read.one("public-ip");
        let wg_endpoints = read.many("wg-endpoint");
        let wg_mtu = read.one("wg-mtu");
        let labels = read.many("label-add");
        let region = read.one("region");
        let accepts = [
            read.one("accepts-builds"),
            read.one("accepts-services"),
            read.one("accepts-ingress"),
        ];
        let network = read.one("network");
        let cluster_name = read.one("cluster-name");
        let version = read.one("version");
        let reset = read.switch("reset");
        let yes = read.switch("yes");
        let ingress_closed = read.switch("ingress-closed");
        let pasted = read.values.into_iter().chain(read.switches).collect();
        Self {
            name,
            storage,
            public_ip,
            wg_endpoints,
            wg_mtu,
            labels,
            region,
            accepts,
            network,
            cluster_name,
            version,
            reset,
            yes,
            ingress_closed,
            pasted,
        }
    }

    pub(crate) fn version(&self) -> String {
        self.version
            .as_ref()
            .expect("version has a default")
            .to_string()
    }

    pub(crate) fn policy(&self) -> Result<super::AskedPolicy, Error> {
        super::AskedPolicy::parse(&self.labels, self.region.as_deref(), self.accepts)
    }

    pub(crate) fn name(&self) -> Result<Option<MachineName>, Error> {
        super::machine::machine_name_requested(self.name.as_deref())
    }

    pub(crate) fn cluster_name(&self) -> Result<Option<ClusterName>, Error> {
        parse_cluster_name(self.cluster_name.as_deref())
    }

    pub(crate) fn storage(&self) -> StoragePreparation {
        // The Cloud path never prompts for storage.
        self.storage
            .clone()
            .unwrap_or(StoragePreparation::Zfs { device: None })
    }

    pub(crate) fn token_request(&self) -> Result<InspectRequest, Error> {
        super::machine::token_request_for(self.public_ip.as_deref(), &self.wg_endpoints)
    }

    pub(crate) fn wg_mtu(&self) -> Option<u32> {
        self.wg_mtu
    }

    pub(crate) fn network(&self) -> Ipv4Net {
        self.network.expect("Cluster network has a default")
    }

    pub(crate) fn reset(&self) -> bool {
        self.reset
    }

    pub(crate) fn yes(&self) -> bool {
        self.yes
    }

    pub(crate) fn ingress_closed(&self) -> bool {
        self.ingress_closed
    }

    /// The flags given here, as `server add` arguments, once each parses as enrollment will
    /// read it.
    pub(crate) fn pasted_args(self) -> Result<Vec<String>, Error> {
        self.policy()?;
        super::machine::checked_token_request_for(self.public_ip.as_deref(), &self.wg_endpoints)?;
        self.name()?;
        self.cluster_name()?;
        Ok(self.pasted)
    }
}

/// What `server add` was asked to enroll: the flags that shape the Server, where to install
/// it, and how it reaches the Server.
pub(crate) struct EnrollPlan {
    shaping: Shaping,
    destination: Option<Destination>,
    no_install: bool,
    show_recovery_code: bool,
    reach: Reach,
}

/// The Server `DESTINATION` names, which `server add` installs over SSH.
struct Destination {
    address: String,
    ssh_key: Option<String>,
    remote: RemoteTarget,
}

/// Where an enrollment reaches the Server it enrolls, and the config it keeps: the command
/// line, as its root matches, with its connection options `--ployz-config`, `--connect`,
/// `--context` and `--ssh-timeout`, and what a rerun keeps.
pub(crate) struct Reach(ArgMatches);

impl EnrollPlan {
    pub(crate) fn from_matches(root: &ArgMatches) -> Self {
        let matches = leaf_matches(root);
        let flag = |id: &str| {
            matches
                .try_get_one::<bool>(id)
                .ok()
                .flatten()
                .copied()
                .unwrap_or(false)
        };
        let text = |id: &str| matches.try_get_one::<String>(id).ok().flatten().cloned();
        Self {
            shaping: Shaping::from_matches(matches),
            destination: text("destination").map(|address| Destination {
                address,
                ssh_key: text("ssh-key"),
                remote: remote_target(matches),
            }),
            no_install: flag("no-install"),
            show_recovery_code: flag("show-recovery-code"),
            reach: Reach(root.clone()),
        }
    }

    pub(crate) fn shaping(&self) -> &Shaping {
        &self.shaping
    }

    /// `DESTINATION`, to provision over SSH.
    pub(crate) fn remote_target(&self) -> Option<&RemoteTarget> {
        self.destination
            .as_ref()
            .map(|destination| &destination.remote)
    }

    pub(crate) fn destination(&self) -> Option<&str> {
        self.destination
            .as_ref()
            .map(|destination| destination.address.as_str())
    }

    pub(crate) fn ssh_key(&self) -> Option<&str> {
        self.destination
            .as_ref()
            .and_then(|destination| destination.ssh_key.as_deref())
    }

    pub(crate) fn no_install(&self) -> bool {
        self.no_install
    }

    pub(crate) fn shows_recovery_code(&self) -> bool {
        self.show_recovery_code
    }

    pub(crate) fn reach(&self) -> &Reach {
        &self.reach
    }
}

impl Reach {
    /// The config this enrollment reads and saves to.
    pub(crate) fn config(&self) -> PathBuf {
        crate::context::expand_home(Path::new(
            leaf_matches(&self.0)
                .get_one::<String>("ployz-config")
                .expect("ployz-config has a default"),
        ))
    }

    /// The connection asked for, which wins over the current context and the local socket.
    pub(crate) fn connect(&self) -> Option<String> {
        crate::cli::connection(leaf_matches(&self.0)).map(str::to_owned)
    }

    pub(crate) fn ssh_timeout(&self) -> Duration {
        crate::cli::ssh_timeout(leaf_matches(&self.0))
    }

    /// `args` as a command that reaches the same Cluster.
    pub(crate) fn rerun(&self, args: &[&str]) -> Rerun {
        Rerun::then(&self.0, args)
    }

    /// This command line again.
    pub(crate) fn again(&self) -> Rerun {
        Rerun::again(&self.0)
    }

    /// The Cluster entry that signs a join waiting for approval.
    pub(crate) async fn entry(&self) -> Result<Client, Error> {
        crate::handlers::route::connect(leaf_matches(&self.0), None).await
    }
}

/// Where `server add` or `server init` installs and how it reaches it.
pub(crate) fn remote_target(matches: &ArgMatches) -> RemoteTarget {
    RemoteTarget::new(
        matches.get_one::<String>("destination").cloned(),
        matches.get_one::<String>("ssh-key").cloned(),
        crate::cli::ssh_timeout(matches),
    )
}

/// The `--cluster-name` value given, as a Cluster Name.
pub(crate) fn parse_cluster_name(name: Option<&str>) -> Result<Option<ClusterName>, Error> {
    name.map(crate::cluster::name::parse).transpose()
}

pub(crate) fn joined_open() {
    crate::ui::warn(
        "--ingress-closed is ignored when joining a Cluster. The Server opens the Cluster's Doors.",
    );
}

pub(crate) fn joined_unnamed() {
    crate::ui::warn(
        "--cluster-name is ignored when joining a Cluster. Rename it with ployz cluster name.",
    );
}
