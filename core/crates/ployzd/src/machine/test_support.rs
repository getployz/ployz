use ployz_core::{
    AdvertisedEndpoint, DeviceKey, EnrollAs, EnrollRequest, JoinPin, Machine, MachineId,
    MachineName, Registered, WireGuardPublicKey,
    authority::{JoinApproval, Signed, Signer, Timestamp},
};

use super::LocalMachineRecord;
use crate::sealing_key::SealingKey;

/// A Cluster a joiner is admitted to: its Sealing Key, the Device that signs joins, and the
/// Machine that admits them. Two Clusters made from different seeds share nothing.
pub(crate) struct ApprovingCluster {
    pub(crate) sealing_key: SealingKey,
    pub(crate) signer: Signer,
    pub(crate) admitting: Machine,
}

impl ApprovingCluster {
    pub(crate) fn new(seed: u8) -> Self {
        Self {
            sealing_key: SealingKey::generate(),
            signer: Signer::from_seed(&[seed; 32]),
            admitting: Machine {
                labels: Default::default(),
                accepts_builds: true,
                accepts_services: true,
                accepts_ingress: true,
                id: MachineId::random(),
                name: MachineName::parse("entry").unwrap(),
                subnet: "10.210.0.0/24".parse().unwrap(),
                public_key: WireGuardPublicKey([seed; 32]),
                public_ip: None,
                advertised_endpoints: vec![AdvertisedEndpoint("192.0.2.1:51820".parse().unwrap())],
                runtime: Default::default(),
                join_approval: None,
                build_concurrency: None,
            },
        }
    }

    pub(crate) fn device(&self) -> DeviceKey {
        self.signer.public().into()
    }

    pub(crate) fn pin(&self) -> JoinPin {
        JoinPin {
            devices: vec![self.device()],
        }
    }

    /// The allocation this Cluster hands `joiner`.
    pub(crate) fn assigned(&self, joiner: &LocalMachineRecord) -> Machine {
        Machine {
            id: joiner.id(),
            name: MachineName::parse("joining").unwrap(),
            subnet: "10.210.1.0/24".parse().unwrap(),
            public_key: joiner.private_key().public_key(),
            advertised_endpoints: vec![AdvertisedEndpoint("192.0.2.2:51820".parse().unwrap())],
            ..self.admitting.clone()
        }
    }

    /// The approval this Cluster's Device signs for `joiner`, trusting only that Device.
    pub(crate) fn approval(&self, joiner: &LocalMachineRecord) -> JoinApproval {
        let assigned = self.assigned(joiner);
        let issued_at = Timestamp::now();
        JoinApproval {
            cluster: self.sealing_key.public_key(),
            machine: (&assigned).into(),
            network: "10.210.0.0/16".parse().unwrap(),
            sealing_recipient: assigned.public_key,
            devices: vec![self.device()],
            admitting_machines: vec![self.admitting.id],
            bootstrap: vec![(&self.admitting).into()],
            issued_at,
            expires_at: issued_at.plus(60_000),
            signer: self.device(),
        }
    }

    /// A registration whose capsule carries `approval`, signed by this Cluster's Device, or
    /// no approval at all.
    pub(crate) fn registration_with(
        &self,
        joiner: &LocalMachineRecord,
        approval: Option<&JoinApproval>,
    ) -> Registered {
        let recipient = joiner.private_key().public_key();
        let sealed = match approval {
            Some(body) => self
                .sealing_key
                .seal_join(&recipient, &Signed::sign(body, &self.signer).unwrap()),
            None => self.sealing_key.seal_to(&recipient),
        };
        Registered {
            assigned_machine: self.assigned(joiner),
            visible_peers: vec![self.admitting.clone()],
            target_versions: [("actor".into(), 4)].into(),
            sealing_key: Some(sealed.unwrap()),
            devices: Vec::new(),
            recipient: None,
        }
    }

    /// A registration under this Cluster's approval of `joiner`.
    pub(crate) fn registration(&self, joiner: &LocalMachineRecord) -> Registered {
        self.registration_with(joiner, Some(&self.approval(joiner)))
    }

    /// The enroll request `joiner` sends to join this Cluster, unpinned as Cloud drives it.
    pub(crate) fn join(&self, joiner: &LocalMachineRecord) -> EnrollRequest {
        member(self.registration(joiner), None)
    }
}

pub(crate) fn member(registration: Registered, pin: Option<JoinPin>) -> EnrollRequest {
    EnrollRequest {
        role: EnrollAs::Member { registration, pin },
        wireguard_mtu: None,
        pinned: Default::default(),
    }
}

/// `machine.json` as a daemon from before enforcement was the only mode wrote it: no
/// `born_enforcing`, no activation, and the switch where a person left it.
pub(crate) fn written_before_f7(dir: &std::path::Path, device_enforcement: bool) {
    let path = dir.join(super::store::STATE_FILE_NAME);
    let mut row: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let row = row.as_object_mut().unwrap();
    assert!(row.remove("born_enforcing").is_some());
    assert!(!row.contains_key("activation"));
    row.insert("device_enforcement".into(), device_enforcement.into());
    std::fs::write(&path, serde_json::to_vec(row).unwrap()).unwrap();
}

/// A Machine record that founded its own Cluster in `dir`, as `ployz server init` leaves it.
pub(crate) fn founded(dir: &std::path::Path) -> super::LocalMachineStore {
    let mut store = super::LocalMachineStore::open(dir).unwrap();
    store
        .enroll(EnrollRequest {
            role: EnrollAs::Founder {
                initial_policy: Default::default(),
                name: MachineName::parse("founder").unwrap(),
                cluster_network: "10.210.0.0/16".parse().unwrap(),
                public_ip: None,
                advertised_endpoints: vec![AdvertisedEndpoint("192.0.2.1:51820".parse().unwrap())],
                grant: None,
            },
            wireguard_mtu: None,
            pinned: Default::default(),
        })
        .unwrap();
    store
}
