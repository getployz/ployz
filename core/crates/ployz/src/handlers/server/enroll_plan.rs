//! What one `ployz server add` asks to enroll, read once from the command line.
//!
//! Every flag that shapes the Server is read here, once, with the text it was given as, so the
//! line `--command` prints repeats exactly the flags the command line or the environment gave
//! and the engine reads the same values. A malformed value is refused when the engine reads it,
//! in the order the engine reads it, so building a plan never fails.

use clap::ArgMatches;
use clap::parser::ValueSource;
use ipnet::Ipv4Net;
use ployz_core::{
    ClusterName, InitialMachinePolicy, InspectRequest, MachineName, MachineRelease,
    StoragePreparation,
};

use crate::handlers::{Error, leaf_matches, string_values};
use crate::provisioning::RemoteTarget;

/// The flags of `server add` that shape the Server.
pub(crate) struct Shaping {
    name: Option<String>,
    storage: Option<StoragePreparation>,
    public_ip: Option<String>,
    wg_endpoints: Vec<String>,
    wg_mtu: Option<u32>,
    labels: Vec<String>,
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

    pub(crate) fn policy(&self) -> Result<InitialMachinePolicy, Error> {
        super::initial_policy(&self.labels, self.accepts)
    }

    pub(crate) fn name(&self) -> Result<Option<MachineName>, Error> {
        super::helpers::machine_name_requested(self.name.as_deref())
    }

    pub(crate) fn cluster_name(&self) -> Result<Option<ClusterName>, Error> {
        super::enroll::parse_cluster_name(self.cluster_name.as_deref())
    }

    pub(crate) fn storage(&self) -> StoragePreparation {
        super::enroll::storage_or_default(self.storage.clone())
    }

    pub(crate) fn token_request(&self) -> Result<InspectRequest, Error> {
        super::helpers::token_request_for(self.public_ip.as_deref(), &self.wg_endpoints)
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
        super::helpers::checked_token_request_for(self.public_ip.as_deref(), &self.wg_endpoints)?;
        self.name()?;
        self.cluster_name()?;
        Ok(self.pasted)
    }
}

/// What `server add` was asked to enroll: the flags that shape the Server, where to install
/// it, and the connection options the command carried.
pub(crate) struct EnrollPlan {
    shaping: Shaping,
    remote: RemoteTarget,
    destination: Option<String>,
    ssh_key: Option<String>,
    no_install: bool,
    show_recovery_code: bool,
    reach: ArgMatches,
}

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
            remote: RemoteTarget::from_matches(matches),
            destination: text("destination"),
            ssh_key: text("ssh-key"),
            no_install: flag("no-install"),
            show_recovery_code: flag("show-recovery-code"),
            reach: matches.clone(),
        }
    }

    pub(crate) fn shaping(&self) -> &Shaping {
        &self.shaping
    }

    pub(crate) fn remote_target(&self) -> &RemoteTarget {
        &self.remote
    }

    pub(crate) fn destination(&self) -> Option<&str> {
        self.destination.as_deref()
    }

    pub(crate) fn ssh_key(&self) -> Option<&str> {
        self.ssh_key.as_deref()
    }

    pub(crate) fn no_install(&self) -> bool {
        self.no_install
    }

    pub(crate) fn shows_recovery_code(&self) -> bool {
        self.show_recovery_code
    }

    /// The connection options of the command (`--ployz-config`, `--connect`, `--context`,
    /// `--ssh-timeout`), which dialing and the commands a hint tells to run read.
    pub(crate) fn reach(&self) -> &ArgMatches {
        &self.reach
    }
}
