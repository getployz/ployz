//! The address each bridge member held, written down before a start detaches any
//! member. Docker forgets a member's address once it is detached, so a start that
//! dies before the member rejoins would leave it to take a free one. The next start
//! gives it back the address written here instead.

use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    net::Ipv4Addr,
    path::{Path, PathBuf},
};

use super::BridgeMember;

const FILE: &str = "bridge-member-addresses.json";

pub(super) struct Pins {
    path: PathBuf,
    addresses: BTreeMap<String, Ipv4Addr>,
}

impl Pins {
    /// Picks up the addresses an earlier start wrote down and never cleared.
    pub(super) fn load(data_dir: &Path) -> Self {
        let path = data_dir.join(FILE);
        let addresses = match std::fs::read(&path) {
            Ok(body) => serde_json::from_slice(&body).unwrap_or_else(|error| {
                eprintln!(
                    "ignoring {path}, which names no addresses: {error}",
                    path = path.display(),
                );
                BTreeMap::new()
            }),
            Err(error) if error.kind() == io::ErrorKind::NotFound => BTreeMap::new(),
            Err(error) => {
                eprintln!(
                    "could not read the addresses an earlier start detached from {path}: {error}",
                    path = path.display(),
                );
                BTreeMap::new()
            }
        };
        Self { path, addresses }
    }

    /// Gives each detached member the address written down for it, unless another
    /// member holds that address now.
    pub(super) fn restore(&self, members: &mut [BridgeMember]) {
        let mut held = members
            .iter()
            .filter_map(|member| member.ipv4)
            .collect::<BTreeSet<_>>();
        for member in members.iter_mut().filter(|member| member.ipv4.is_none()) {
            if let Some(&ipv4) = self.addresses.get(&member.id)
                && held.insert(ipv4)
            {
                member.ipv4 = Some(ipv4);
            }
        }
    }

    /// Writes down the address of every member that has one. Detach a member only
    /// once this succeeds.
    pub(super) fn record(&mut self, members: &[BridgeMember]) -> io::Result<()> {
        let before = self.addresses.clone();
        self.addresses.extend(
            members
                .iter()
                .filter_map(|member| Some((member.id.clone(), member.ipv4?))),
        );
        if self.addresses == before {
            return Ok(());
        }
        let body = serde_json::to_vec(&self.addresses).map_err(io::Error::other)?;
        crate::filesystem::atomic_write(&self.path, &body, 0o600).inspect_err(|_| {
            self.addresses = before;
        })
    }

    /// Clears the note once every member has rejoined, keeping the addresses of
    /// `unjoined` for the next start.
    pub(super) fn finish(mut self, unjoined: &BTreeSet<String>) {
        self.addresses.retain(|id, _| unjoined.contains(id));
        let written = if self.addresses.is_empty() {
            match std::fs::remove_file(&self.path) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                removed => removed,
            }
        } else {
            serde_json::to_vec(&self.addresses)
                .map_err(io::Error::other)
                .and_then(|body| crate::filesystem::atomic_write(&self.path, &body, 0o600))
        };
        if let Err(error) = written {
            eprintln!(
                "could not update {path}: {error}",
                path = self.path.display(),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(id: &str, ipv4: Option<[u8; 4]>, attached: bool) -> BridgeMember {
        BridgeMember {
            id: id.into(),
            ipv4: ipv4.map(Ipv4Addr::from),
            attached,
            pinned: attached,
            ipv6: false,
        }
    }

    #[test]
    fn a_start_that_died_mid_readdress_gives_detached_members_their_addresses_back() {
        let dir = tempfile::tempdir().unwrap();
        let mut pins = Pins::load(dir.path());
        pins.record(&[
            member("ingress", Some([10, 210, 0, 2]), true),
            member("web", Some([10, 210, 0, 14]), true),
            member("egress", Some([10, 210, 0, 15]), true),
        ])
        .unwrap();
        drop(pins);

        // The start died after detaching ingress and web.
        let mut members = [
            member("egress", Some([10, 210, 0, 15]), true),
            member("ingress", None, false),
            member("web", None, false),
            member("new", None, false),
        ];
        Pins::load(dir.path()).restore(&mut members);
        let addresses = members.map(|member| member.ipv4);
        assert_eq!(
            addresses,
            [
                Some([10, 210, 0, 15].into()),
                Some([10, 210, 0, 2].into()),
                Some([10, 210, 0, 14].into()),
                None,
            ]
        );
    }

    #[test]
    fn an_address_another_member_took_meanwhile_is_not_given_back() {
        let dir = tempfile::tempdir().unwrap();
        let mut pins = Pins::load(dir.path());
        pins.record(&[member("web", Some([10, 210, 0, 14]), true)])
            .unwrap();
        let mut members = [
            member("web", None, false),
            member("squatter", Some([10, 210, 0, 14]), true),
        ];
        pins.restore(&mut members);
        assert_eq!(members[0].ipv4, None);
    }

    #[test]
    fn the_note_goes_once_every_member_rejoined_and_keeps_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let mut pins = Pins::load(dir.path());
        pins.record(&[
            member("web", Some([10, 210, 0, 14]), true),
            member("stuck", Some([10, 210, 0, 9]), true),
        ])
        .unwrap();
        pins.finish(&BTreeSet::from(["stuck".to_owned()]));
        assert_eq!(
            Pins::load(dir.path()).addresses,
            BTreeMap::from([("stuck".to_owned(), Ipv4Addr::new(10, 210, 0, 9))])
        );
        Pins::load(dir.path()).finish(&BTreeSet::new());
        assert!(!dir.path().join(FILE).exists());
    }

    #[test]
    fn a_start_that_detached_nothing_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let mut pins = Pins::load(dir.path());
        pins.record(&[member("detached", None, false)]).unwrap();
        assert!(!dir.path().join(FILE).exists());
    }
}
