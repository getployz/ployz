//! What one `ployz server add` asks to enroll, read once from the command line or from the
//! enroll instruction Cloud sends the check-in loop.
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
use ployz_core::{
    ClusterName, InspectRequest, MachineLayout, MachineName, MachineRelease, StoragePreparation,
    ValueError, ZfsDevice,
};

use crate::checkin::{EnrollInstruction, EnrollStorage};
use crate::cli::{leaf_matches, string_values};
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

/// Where an enrollment reaches the Server it enrolls, and the config it keeps.
pub(crate) enum Reach {
    /// The command's connection options: `--ployz-config`, `--connect`, `--context` and
    /// `--ssh-timeout`.
    CommandLine(ArgMatches),
    /// This host's daemon over its socket, as the check-in loop enrolls it.
    ThisHost { config: PathBuf },
}

/// A value in Cloud's enroll instruction that `server add` refuses before it enrolls.
#[derive(Debug, thiserror::Error)]
pub(crate) enum EnrollInstructionError {
    #[error("Cloud asked for the {field} {value:?}, which can't start with '-'.")]
    LeadingDash { field: &'static str, value: String },
    #[error("Cloud asked for ZFS on the device {device:?}.")]
    ZfsDevice {
        device: String,
        #[source]
        source: ValueError,
    },
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
            destination: text("destination").map(|address| Destination {
                address,
                ssh_key: text("ssh-key"),
                remote: crate::handlers::server::provision::remote_target(matches),
            }),
            no_install: flag("no-install"),
            show_recovery_code: flag("show-recovery-code"),
            reach: Reach::CommandLine(matches.clone()),
        }
    }

    /// What Cloud's enroll `instruction` asks of this host: `server add --yes` through its
    /// daemon's socket, keeping `config`. A value `server add` refuses as a flag, such as one
    /// starting with `-`, is refused here; one the engine refuses is left to the engine.
    pub(crate) fn from_instruction(
        instruction: &EnrollInstruction,
        config: PathBuf,
    ) -> Result<Self, EnrollInstructionError> {
        let unflagged = |field: &'static str, value: &str| {
            if value.starts_with('-') && value != "-" {
                Err(EnrollInstructionError::LeadingDash {
                    field,
                    value: value.to_owned(),
                })
            } else {
                Ok(value.to_owned())
            }
        };
        let storage = match &instruction.storage {
            EnrollStorage::None => StoragePreparation::None,
            EnrollStorage::Zfs { device } => StoragePreparation::Zfs {
                device: device
                    .as_deref()
                    .map(|device| {
                        ZfsDevice::parse(device).map_err(|source| {
                            EnrollInstructionError::ZfsDevice {
                                device: device.to_owned(),
                                source,
                            }
                        })
                    })
                    .transpose()?,
            },
        };
        let policy = &instruction.policy;
        let mut labels = Vec::new();
        for (key, value) in &policy.labels {
            let label = unflagged("label", &format!("{key}={value}"))?;
            // `--label-add` splits its value at commas.
            labels.extend(label.split(',').map(str::to_owned));
        }
        let shaping = Shaping {
            name: (!instruction.name.is_empty())
                .then(|| unflagged("Server name", &instruction.name))
                .transpose()?,
            storage: Some(storage),
            public_ip: Some("auto".to_owned()),
            wg_endpoints: Vec::new(),
            wg_mtu: None,
            labels,
            region: None,
            accepts: [
                Some(policy.accepts_builds),
                Some(policy.accepts_services),
                Some(policy.accepts_ingress),
            ],
            network: Some(ployz_core::DEFAULT_CLUSTER_NETWORK),
            cluster_name: instruction
                .founding_cluster_name
                .as_deref()
                .map(|name| unflagged("Cluster name", name))
                .transpose()?,
            version: Some(
                env!("CARGO_PKG_VERSION")
                    .parse()
                    .expect("this CLI's release parses"),
            ),
            reset: false,
            yes: true,
            ingress_closed: instruction.founding_ingress_closed,
            pasted: Vec::new(),
        };
        Ok(Self {
            shaping,
            destination: None,
            no_install: false,
            show_recovery_code: false,
            reach: Reach::ThisHost { config },
        })
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
        match self {
            Self::CommandLine(matches) => crate::context::expand_home(Path::new(
                matches
                    .get_one::<String>("ployz-config")
                    .expect("ployz-config has a default"),
            )),
            Self::ThisHost { config } => config.clone(),
        }
    }

    /// The connection asked for, which wins over the current context and the local socket.
    pub(crate) fn connect(&self) -> Option<String> {
        match self {
            Self::CommandLine(matches) => matches.get_one::<String>("connect").cloned(),
            Self::ThisHost { .. } => Some(this_host()),
        }
    }

    pub(crate) fn ssh_timeout(&self) -> Duration {
        match self {
            Self::CommandLine(matches) => crate::cli::ssh_timeout(matches),
            Self::ThisHost { .. } => crate::connect::DEFAULT_SSH_TIMEOUT,
        }
    }

    /// `args` as a command that reaches the same Cluster.
    pub(crate) fn rerun(&self, args: &[&str]) -> String {
        match self {
            Self::CommandLine(matches) => crate::cli::rerun(matches, args),
            Self::ThisHost { config } => shell_words::join(
                [
                    "ployz",
                    "--ployz-config",
                    &config.to_string_lossy(),
                    "--connect",
                    &this_host(),
                ]
                .into_iter()
                .chain(args.iter().copied()),
            ),
        }
    }

    /// The Cluster entry that signs a join waiting for approval.
    pub(crate) async fn entry(&self) -> Result<Client, Error> {
        match self {
            Self::CommandLine(matches) => crate::handlers::route::connect(matches, None).await,
            Self::ThisHost { config } => Ok(crate::connect::connect_with_ssh_timeout(
                config,
                Some(&this_host()),
                None,
                crate::connect::DEFAULT_SSH_TIMEOUT,
            )
            .await?),
        }
    }
}

/// This host's daemon socket, as a `--connect` value.
fn this_host() -> String {
    format!("unix://{}", MachineLayout::production().socket().display())
}

/// The `--cluster-name` value given, as a Cluster Name.
pub(crate) fn parse_cluster_name(name: Option<&str>) -> Result<Option<ClusterName>, Error> {
    name.map(crate::handlers::cluster::parse).transpose()
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
