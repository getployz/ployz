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
            passkeys: Vec::new(),
            admitting_machines: vec![self.admitting.id],
            bootstrap: vec![(&self.admitting).into()],
            issued_at,
            expires_at: issued_at.plus(60_000),
            signer: self.device().into(),
            proposal: None,
            grants: Vec::new(),
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
            Some(body) => self.sealing_key.seal_join(
                &recipient,
                &Signed::sign(body, &self.signer).unwrap(),
                None,
                Some(&trust_of(body)),
            ),
            None => self.sealing_key.seal_to(&recipient),
        };
        Registered {
            assigned_machine: self.assigned(joiner),
            visible_peers: vec![self.admitting.clone()],
            target_versions: [("actor".into(), 4)].into(),
            sealing_key: Some(sealed.unwrap()),
            devices: Vec::new(),
            grants: Vec::new(),
            restrictions: None,
        }
    }

    /// A registration under this Cluster's approval of `joiner`.
    pub(crate) fn registration(&self, joiner: &LocalMachineRecord) -> Registered {
        self.registration_with(joiner, Some(&self.approval(joiner)))
    }

    /// The enroll request `joiner` sends to join this Cluster, pinned to its Device.
    pub(crate) fn join(&self, joiner: &LocalMachineRecord) -> EnrollRequest {
        member(self.registration(joiner), Some(self.pin()))
    }
}

/// The trust an admitting Machine that holds exactly `body`'s keys seals beside it.
pub(crate) fn trust_of(body: &JoinApproval) -> crate::sealing_key::JoinTrust {
    crate::sealing_key::JoinTrust {
        devices: body.devices.clone(),
        passkeys: body.passkeys.clone(),
        grants: body.grants.clone(),
        ..Default::default()
    }
}

/// A passkey of `seed`'s account, as an admitting Server would list it in an approval.
pub(crate) fn passkey(seed: u8) -> ployz_core::authority::Passkey {
    use ployz_core::authority::{
        Passkey, PasskeyAccount, PasskeyPublicKey,
        trusted_key::{Base64Url, PasskeyAlgorithm, RelyingParty},
    };
    let key = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]).verifying_key();
    Passkey {
        credential_id: Base64Url::new(vec![seed, 1, 2, 3]).try_into().unwrap(),
        public_key: PasskeyPublicKey::parse(PasskeyAlgorithm::Eddsa, key.as_bytes()).unwrap(),
        account: PasskeyAccount::parse("ada@example.com").unwrap(),
        relying_party: RelyingParty::of(&"https://ployz.cloud".parse().unwrap()).unwrap(),
    }
}

pub(crate) fn member(registration: Registered, pin: Option<JoinPin>) -> EnrollRequest {
    EnrollRequest {
        role: EnrollAs::Member { registration, pin },
        wireguard_mtu: None,
        pinned: Default::default(),
    }
}

/// The pin a CLI holding the admitting Server's Devices sends with `registration`.
pub(crate) fn pin_of(registration: &Registered) -> Option<JoinPin> {
    Some(JoinPin {
        devices: registration.devices.clone(),
    })
}

/// A store at `dir` that founded its own Cluster as `name`, so it is Participating.
pub(crate) fn founder(dir: &std::path::Path, name: &str) -> super::LocalMachineStore {
    let mut store = super::LocalMachineStore::open(dir).unwrap();
    store
        .enroll(EnrollRequest {
            role: EnrollAs::Founder {
                initial_policy: Default::default(),
                name: MachineName::parse(name).unwrap(),
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

/// Trust two Devices in `store` and return the first, whose key also holds Cloud's slot, as
/// a key Cloud also connects with does: removing its Device leaves the key an Operator's.
/// The second keeps the Machine from losing its last key.
pub(crate) fn device_in_two_slots(store: &mut super::LocalMachineStore) -> DeviceKey {
    let device = DeviceKey::from(Signer::from_seed(&[7; 32]).public());
    store.approve_device(device).unwrap();
    store
        .approve_device(DeviceKey::from(Signer::from_seed(&[9; 32]).public()))
        .unwrap();
    store
        .stage_management_client(ployz_core::ManagementClientLabel::cloud(), device.into())
        .unwrap();
    store.activate_management_client(device.into()).unwrap();
    device
}
