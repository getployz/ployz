//! The spec file ployzd writes for `ployzd dns`.
//!
//! `/run/ployz/dns.json` is the only channel between the daemon and the DNS
//! process. The daemon publishes one spec while the local Machine participates
//! and removes it otherwise. The process reads it at start and on every tick,
//! and exits when the spec it serves changes.

use std::{
    fs, io,
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    path::{Path, PathBuf},
};

use ipnet::Ipv4Net;
use ployz_core::{CORROSION_API_PORT, MachineId};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::PORT;
use crate::{
    corrosion::CorrosionPaths,
    filesystem::atomic_write,
    machine::{LocalMachineBody, LocalMachineRecord},
};

const FILE_NAME: &str = "dns.json";

/// Everything one `ployzd dns` process needs to serve one participating Machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DnsSpec {
    pub(crate) machine: MachineId,
    pub(crate) listen: SocketAddrV4,
    pub(crate) local_subnet: Ipv4Net,
    /// Explicit forwarders; empty means follow `/etc/resolv.conf`.
    pub(crate) upstreams: Vec<SocketAddr>,
    pub(crate) corrosion: CorrosionEndpoint,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CorrosionEndpoint {
    pub(crate) api: SocketAddr,
    pub(crate) admin_socket: PathBuf,
    pub(crate) token_file: PathBuf,
}

impl DnsSpec {
    /// The spec for this record, or `None` while the Machine does not participate.
    pub(crate) fn of(
        record: &LocalMachineRecord,
        upstreams: &[SocketAddr],
        data_dir: &Path,
        run_dir: &Path,
    ) -> Option<Self> {
        let LocalMachineBody::Participating { machine, .. } = record.body() else {
            return None;
        };
        let paths = CorrosionPaths::under(data_dir, run_dir);
        Some(Self {
            machine: machine.id,
            listen: SocketAddrV4::new(machine.subnet.gateway().0, PORT),
            local_subnet: machine.subnet.into(),
            upstreams: upstreams.to_vec(),
            corrosion: CorrosionEndpoint {
                api: SocketAddr::from((Ipv4Addr::LOCALHOST, CORROSION_API_PORT)),
                admin_socket: paths.admin_socket(),
                token_file: paths.token_file(),
            },
        })
    }
}

#[derive(Debug, Error)]
enum InvalidSpec {
    #[error("listen address {listen} is outside the Machine subnet {subnet}")]
    ListenOutsideSubnet { listen: Ipv4Addr, subnet: Ipv4Net },
    #[error("listen port {0} is not {PORT}")]
    ListenPort(u16),
}

#[derive(Serialize, Deserialize)]
struct Wire {
    machine: MachineId,
    listen: SocketAddrV4,
    subnet: Ipv4Net,
    #[serde(default)]
    upstreams: Vec<SocketAddr>,
    corrosion_api: SocketAddr,
    corrosion_admin_socket: PathBuf,
    corrosion_token_file: PathBuf,
}

impl From<&DnsSpec> for Wire {
    fn from(spec: &DnsSpec) -> Self {
        Self {
            machine: spec.machine,
            listen: spec.listen,
            subnet: spec.local_subnet,
            upstreams: spec.upstreams.clone(),
            corrosion_api: spec.corrosion.api,
            corrosion_admin_socket: spec.corrosion.admin_socket.clone(),
            corrosion_token_file: spec.corrosion.token_file.clone(),
        }
    }
}

impl TryFrom<Wire> for DnsSpec {
    type Error = InvalidSpec;

    fn try_from(wire: Wire) -> Result<Self, InvalidSpec> {
        if !wire.subnet.contains(wire.listen.ip()) {
            return Err(InvalidSpec::ListenOutsideSubnet {
                listen: *wire.listen.ip(),
                subnet: wire.subnet,
            });
        }
        if wire.listen.port() != PORT {
            return Err(InvalidSpec::ListenPort(wire.listen.port()));
        }
        Ok(Self {
            machine: wire.machine,
            listen: wire.listen,
            local_subnet: wire.subnet,
            upstreams: wire.upstreams,
            corrosion: CorrosionEndpoint {
                api: wire.corrosion_api,
                admin_socket: wire.corrosion_admin_socket,
                token_file: wire.corrosion_token_file,
            },
        })
    }
}

/// `dns.json` in one run directory.
#[derive(Clone, Debug)]
pub(crate) struct SpecFile {
    path: PathBuf,
}

impl SpecFile {
    pub(crate) fn in_run_dir(run_dir: &Path) -> Self {
        Self {
            path: run_dir.join(FILE_NAME),
        }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Write `spec`, or remove the file for `None`. `Ok(false)` when nothing changed.
    pub(crate) fn publish(&self, spec: Option<&DnsSpec>) -> io::Result<bool> {
        match spec {
            Some(spec) => {
                let bytes = serde_json::to_vec_pretty(&Wire::from(spec))?;
                if fs::read(&self.path).is_ok_and(|current| current == bytes) {
                    return Ok(false);
                }
                atomic_write(&self.path, &bytes, 0o644)?;
                Ok(true)
            }
            None => match fs::remove_file(&self.path) {
                Ok(()) => Ok(true),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
                Err(error) => Err(error),
            },
        }
    }

    /// The published spec, `None` when the file is absent.
    pub(crate) fn read(&self) -> io::Result<Option<DnsSpec>> {
        let bytes = match fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let wire: Wire = serde_json::from_slice(&bytes)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        DnsSpec::try_from(wire)
            .map(Some)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
    }
}

#[cfg(test)]
mod tests {
    use ployz_core::Machine;
    use serde_json::json;

    use super::*;
    use crate::{
        machine::{LocalMachineBody, ParticipationOrigin},
        network::WireGuardPrivateKey,
    };

    fn machine() -> Machine {
        serde_json::from_value(json!({
            "id": "b".repeat(32),
            "name": "machine",
            "subnet": "10.210.3.0/24",
            "labels": {}, "accepts_builds": true, "accepts_services": true, "accepts_ingress": true,
            "public_key": WireGuardPrivateKey::from_bytes([0; 32]).public_key(),
            "advertised_endpoints": ["192.0.2.1:51820"],
        }))
        .unwrap()
    }

    fn record(body: LocalMachineBody) -> LocalMachineRecord {
        LocalMachineRecord::parse(body, WireGuardPrivateKey::from_bytes([0; 32])).unwrap()
    }

    fn participating() -> LocalMachineRecord {
        let machine = machine();
        record(LocalMachineBody::Participating {
            machine: machine.clone(),
            origin: ParticipationOrigin::Join {
                bootstrap: vec![machine],
            },
        })
    }

    fn spec() -> DnsSpec {
        DnsSpec::of(
            &participating(),
            &["192.0.2.53:53".parse().unwrap()],
            Path::new("/var/lib/ployz"),
            Path::new("/run/ployz"),
        )
        .unwrap()
    }

    #[test]
    fn spec_exists_only_while_participating() {
        let spec = spec();
        assert_eq!(spec.machine, MachineId::parse("b".repeat(32)).unwrap());
        assert_eq!(spec.listen, "10.210.3.1:53".parse().unwrap());
        assert_eq!(spec.local_subnet, "10.210.3.0/24".parse().unwrap());
        assert_eq!(spec.upstreams, vec!["192.0.2.53:53".parse().unwrap()]);
        assert_eq!(spec.corrosion.api, "127.0.0.1:7571".parse().unwrap());
        assert_eq!(
            spec.corrosion.admin_socket,
            Path::new("/run/ployz/corrosion/admin.sock")
        );
        assert_eq!(
            spec.corrosion.token_file,
            Path::new("/var/lib/ployz/corrosion/.api-token")
        );

        let machine = machine();
        let joining = record(LocalMachineBody::Joining {
            machine: machine.clone(),
            bootstrap: vec![machine],
            min_store_version: Default::default(),
        });
        assert_eq!(
            DnsSpec::of(&joining, &[], Path::new("/d"), Path::new("/r")),
            None
        );
    }

    #[test]
    fn publish_skips_equal_bytes_and_none_removes() {
        let dir = tempfile::tempdir().unwrap();
        let file = SpecFile::in_run_dir(dir.path());
        assert!(file.publish(Some(&spec())).unwrap());
        assert!(!file.publish(Some(&spec())).unwrap());
        assert_eq!(file.read().unwrap(), Some(spec()));
        assert!(file.publish(None).unwrap());
        assert!(!file.publish(None).unwrap());
        assert_eq!(file.read().unwrap(), None);
    }

    #[test]
    fn read_ignores_unknown_fields_and_defaults_upstreams() {
        let dir = tempfile::tempdir().unwrap();
        let file = SpecFile::in_run_dir(dir.path());
        fs::write(
            file.path(),
            json!({
                "machine": "b".repeat(32),
                "listen": "10.210.3.1:53",
                "subnet": "10.210.3.0/24",
                "corrosion_api": "127.0.0.1:7571",
                "corrosion_admin_socket": "/run/ployz/corrosion/admin.sock",
                "corrosion_token_file": "/var/lib/ployz/corrosion/.api-token",
                "future": true,
            })
            .to_string(),
        )
        .unwrap();
        let read = file.read().unwrap().unwrap();
        assert_eq!(read.upstreams, Vec::<SocketAddr>::new());
        assert_eq!(read.listen, "10.210.3.1:53".parse().unwrap());
    }

    #[test]
    fn read_rejects_listen_outside_subnet_or_off_port() {
        let dir = tempfile::tempdir().unwrap();
        let file = SpecFile::in_run_dir(dir.path());
        for (listen, message) in [
            ("10.210.4.1:53", "outside the Machine subnet"),
            ("10.210.3.1:5353", "listen port 5353 is not 53"),
        ] {
            fs::write(
                file.path(),
                json!({
                    "machine": "b".repeat(32),
                    "listen": listen,
                    "subnet": "10.210.3.0/24",
                    "corrosion_api": "127.0.0.1:7571",
                    "corrosion_admin_socket": "/a",
                    "corrosion_token_file": "/t",
                })
                .to_string(),
            )
            .unwrap();
            let error = file.read().unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            assert!(error.to_string().contains(message), "{error}");
        }
    }
}
