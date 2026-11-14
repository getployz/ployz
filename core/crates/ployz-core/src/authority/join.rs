//! Device authority for admitting one Machine and releasing the Cluster's Sealing Key.

use crate::{AdvertisedEndpoint, InitialMachinePolicy, MachineName, MachineSubnet};
use ipnet::Ipv4Net;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use ts_rs::TS;

use super::{
    AuthorityRefusal, Purpose, Signed, SigningPublicKey, Statement, Timestamp,
    refusal::{field, first_mismatch},
};
use crate::{DeviceKey, Machine, MachineId, SealingPublicKey, WireGuardPublicKey};

/// The immutable enrollment allocation. Runtime observations confer no authority.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct JoinAllocation {
    pub id: MachineId,
    pub name: MachineName,
    pub subnet: MachineSubnet,
    pub public_key: WireGuardPublicKey,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "string")]
    pub public_ip: Option<std::net::IpAddr>,
    pub advertised_endpoints: Vec<AdvertisedEndpoint>,
    pub policy: InitialMachinePolicy,
}

impl From<&Machine> for JoinAllocation {
    fn from(machine: &Machine) -> Self {
        Self {
            id: machine.id,
            name: machine.name.clone(),
            subnet: machine.subnet,
            public_key: machine.public_key,
            public_ip: machine.public_ip,
            advertised_endpoints: machine.advertised_endpoints.clone(),
            policy: InitialMachinePolicy {
                labels: machine.labels.clone(),
                accepts_builds: machine.accepts_builds,
                accepts_services: machine.accepts_services,
                accepts_ingress: machine.accepts_ingress,
            },
        }
    }
}

impl JoinAllocation {
    /// Whether this allocation preserves the identity and policy the CLI requested.
    #[must_use]
    pub fn matches_request(&self, request: &crate::RegisterRequest) -> bool {
        self.id == request.machine_id
            && self.name == request.name
            && request
                .assigned_subnet
                .is_none_or(|subnet| self.subnet == subnet)
            && self.public_key == request.public_key
            && self.public_ip == request.public_ip
            && self.advertised_endpoints == request.advertised_endpoints
            && self.policy == request.initial_policy
    }
}

/// The code identifies both the Machine's durable ID and its locally generated key.
#[must_use]
pub fn code(machine: MachineId, key: WireGuardPublicKey) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ployz-join-v1\n");
    digest.update(machine.as_str().as_bytes());
    digest.update(key.0);
    let digest = digest.finalize();
    let bits = u128::from_be_bytes(
        *digest
            .as_slice()
            .first_chunk()
            .expect("SHA-256 has 16 bytes"),
    );
    let alphabet = b"abcdefghijklmnopqrstuvwxyz234567";
    (0..20)
        .map(|index| {
            let digit = usize::try_from((bits >> (128 - 5 * (index + 1))) & 31).expect("five bits");
            char::from(*alphabet.get(digit).expect("five bits index 32 digits"))
        })
        .collect()
}

/// One Device's approval of an allocation, its key recipient and initial trusted Devices.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct JoinApproval {
    pub cluster: SealingPublicKey,
    pub machine: JoinAllocation,
    #[ts(type = "string")]
    pub network: Ipv4Net,
    /// Currently the WireGuard key also receives the Sealing Key.
    pub sealing_recipient: WireGuardPublicKey,
    pub devices: Vec<DeviceKey>,
    /// The passkeys the admitting Machine trusts, so a Server that joins later approves what
    /// the first one does. A passkey installed from here approves workflows and nothing else.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(optional, as = "Option<Vec<super::Passkey>>")]
    pub passkeys: Vec<super::Passkey>,
    /// Machine IDs scope the approval even when two Clusters share a Sealing key.
    pub admitting_machines: Vec<MachineId>,
    pub bootstrap: Vec<JoinAllocation>,
    pub issued_at: Timestamp,
    pub expires_at: Timestamp,
    pub signer: DeviceKey,
}

impl Statement for JoinApproval {
    const PURPOSE: Purpose = Purpose::Join;
}

impl JoinApproval {
    /// The code the joining Server prints for a person to compare before approval.
    #[must_use]
    pub fn code(&self) -> String {
        code(self.machine.id, self.machine.public_key)
    }
}

/// The joining Machine's proof that it opened its approved key capsule: a MAC under the
/// Cluster's Sealing Key over the signed approval. Only a holder of that key can make one, so
/// an admitting Machine publishes the joiner's row only after seeing it. A joiner Cloud cut off
/// from its capsule never becomes a peer.
#[derive(Clone, Copy, Debug, Eq, PartialEq, TS)]
#[ts(type = "string")]
pub struct JoinConfirmation(pub [u8; 32]);

impl Serialize for JoinConfirmation {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(&format_args!("{self}"))
    }
}

impl<'de> Deserialize<'de> for JoinConfirmation {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        super::canonical::hex_bytes(&value)
            .map(Self)
            .ok_or_else(|| {
                serde::de::Error::custom("a join confirmation must be 64 lowercase hex digits")
            })
    }
}

impl std::fmt::Display for JoinConfirmation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        super::canonical::write_hex(&self.0, formatter)
    }
}

/// Verify a join at an admitting Machine using only its own trusted keys and identity. The
/// joiner trusts the approval's Devices and passkeys, so each must already be trusted here.
///
/// # Errors
/// Refuses untrusted signatures, expiry and any allocation, recipient or Cluster mismatch.
#[expect(
    clippy::too_many_arguments,
    reason = "each argument is one fact the admitting Machine holds itself"
)]
pub fn verify(
    signed: &Signed<JoinApproval>,
    cluster: SealingPublicKey,
    admitting: MachineId,
    devices: &[DeviceKey],
    passkeys: &[super::Passkey],
    machine: &JoinAllocation,
    network: Ipv4Net,
    now: Timestamp,
) -> Result<JoinApproval, AuthorityRefusal> {
    let body = verify_signature(signed, devices, now)?;
    first_mismatch([
        (field::CLUSTER, body.cluster == cluster),
        (
            "approved Devices",
            body.devices.contains(&body.signer)
                && body.devices.iter().all(|key| devices.contains(key)),
        ),
        (
            "approved passkeys",
            body.passkeys
                .iter()
                .all(|passkey| passkeys.contains(passkey)),
        ),
        (
            "an admitting Machine",
            body.admitting_machines.contains(&admitting),
        ),
        (
            "a Machine allocation",
            &body.machine == machine && body.network == network,
        ),
        (
            "a sealing recipient",
            body.sealing_recipient == machine.public_key,
        ),
    ])?;
    Ok(body)
}

/// A join approval a joining Machine accepted, with the Devices it trusts from it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinerApproval {
    pub body: JoinApproval,
    /// The Devices of `body` the joiner trusts: all of them with no pin, and with one only
    /// those the pin also names, so Cloud cannot add a Device of its own beside the person's.
    pub devices: Vec<DeviceKey>,
}

/// Verify a join at the joining Machine, which holds no keys of its own yet. `pinned` are the
/// Devices the person named; with none, the approval's own Devices are trusted, so the
/// capsule it travelled in is what vouches for it. `cluster` is the Sealing Key the capsule
/// opened to, and `machine` the allocation the joiner was handed.
///
/// # Errors
/// Refuses an unreadable body, an untrusted signature, expiry, any allocation, recipient,
/// Cluster, network or admitting-Machine mismatch, and a signer outside the Devices the
/// joiner trusts from the approval.
pub fn verify_as_joiner(
    signed: &Signed<JoinApproval>,
    pinned: Option<&[DeviceKey]>,
    machine: &Machine,
    cluster: Option<SealingPublicKey>,
    now: Timestamp,
) -> Result<JoinerApproval, AuthorityRefusal> {
    let own = signed.open()?;
    let body = verify_signature(signed, pinned.unwrap_or(&own.devices), now)?;
    let devices: Vec<DeviceKey> = body
        .devices
        .iter()
        .copied()
        .filter(|device| pinned.is_none_or(|pinned| pinned.contains(device)))
        .collect();
    if body.machine != JoinAllocation::from(machine)
        || body.sealing_recipient != machine.public_key
        || cluster != Some(body.cluster)
        || !body.network.contains(&Ipv4Net::from(machine.subnet))
        || body.admitting_machines.is_empty()
    {
        return Err(AuthorityRefusal::Mismatch {
            field: "a joining Machine",
        });
    }
    if !devices.contains(&body.signer) {
        return Err(AuthorityRefusal::Mismatch {
            field: "approved Devices",
        });
    }
    Ok(JoinerApproval { body, devices })
}

/// Whether `row` carries a join approval that one of `devices` signed for exactly this Machine
/// in `cluster`. An observation about a replicated row: it never authorizes an effect, so a
/// row it refuses is left out and nothing it admits is acted on without a check of its own.
#[must_use]
pub fn admits_row(
    row: &Machine,
    devices: impl IntoIterator<Item = DeviceKey>,
    cluster: Option<SealingPublicKey>,
) -> bool {
    let Some(signed) = &row.join_approval else {
        return false;
    };
    let Ok(signers) = signed.verified_signers(devices.into_iter().map(SigningPublicKey::from))
    else {
        return false;
    };
    let Ok(body) = signed.open() else {
        return false;
    };
    super::Member::from(&body.machine).matches(row)
        && body.sealing_recipient == row.public_key
        && signers.contains(&body.signer.into())
        && cluster == Some(body.cluster)
}

/// Verify the signature and expiry against pinned Devices.
///
/// # Errors
/// Refuses a signer not among `devices`, malformed statements and expired approvals.
pub fn verify_signature(
    signed: &Signed<JoinApproval>,
    devices: &[DeviceKey],
    now: Timestamp,
) -> Result<JoinApproval, AuthorityRefusal> {
    let signers = signed.verified_signers(devices.iter().copied().map(SigningPublicKey::from))?;
    let body = signed.open()?;
    if !signers.contains(&body.signer.into()) {
        return Err(AuthorityRefusal::Mismatch {
            field: "the signer",
        });
    }
    if body.admitting_machines.len() != 1 {
        return Err(AuthorityRefusal::Mismatch {
            field: "one admitting Machine",
        });
    }
    super::validity::verify(body.issued_at, body.expires_at, now)?;
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authority::Signer;

    const NOW: u64 = 1_000_000;

    fn now() -> Timestamp {
        Timestamp::from_millis(NOW).unwrap()
    }

    fn signer(seed: u8) -> Signer {
        Signer::from_seed(&[seed; 32])
    }

    fn device(seed: u8) -> DeviceKey {
        signer(seed).public().into()
    }

    fn cluster(seed: u8) -> SealingPublicKey {
        SealingPublicKey::from_bytes([seed; 32])
    }

    fn machine(name: &str, subnet: &str, key: u8) -> Machine {
        Machine {
            labels: Default::default(),
            accepts_builds: true,
            accepts_services: true,
            accepts_ingress: true,
            id: MachineId::parse("a".repeat(32)).unwrap(),
            name: MachineName::parse(name).unwrap(),
            subnet: subnet.parse().unwrap(),
            public_key: WireGuardPublicKey([key; 32]),
            public_ip: None,
            advertised_endpoints: vec![AdvertisedEndpoint("192.0.2.2:51820".parse().unwrap())],
            runtime: Default::default(),
            join_approval: None,
            build_concurrency: None,
        }
    }

    /// The approval Device 1 signs for `joiner` into Cluster 5, admitted by one Machine.
    fn approval(joiner: &Machine) -> JoinApproval {
        JoinApproval {
            cluster: cluster(5),
            machine: joiner.into(),
            network: "10.210.0.0/16".parse().unwrap(),
            sealing_recipient: joiner.public_key,
            devices: vec![device(1)],
            passkeys: Vec::new(),
            admitting_machines: vec![MachineId::parse("b".repeat(32)).unwrap()],
            bootstrap: vec![(&machine("entry", "10.210.0.0/24", 9)).into()],
            issued_at: now(),
            expires_at: now().plus(60_000),
            signer: device(1),
        }
    }

    fn sign(body: &JoinApproval, seed: u8) -> Signed<JoinApproval> {
        Signed::sign(body, &signer(seed)).unwrap()
    }

    fn joiner() -> Machine {
        machine("joining", "10.210.1.0/24", 7)
    }

    #[test]
    fn a_joiner_admits_an_approval_for_its_own_allocation_pinned_or_not() {
        let joiner = joiner();
        let signed = sign(&approval(&joiner), 1);

        for pinned in [None, Some(&[device(1)][..])] {
            let joined =
                verify_as_joiner(&signed, pinned, &joiner, Some(cluster(5)), now()).unwrap();
            assert_eq!(joined.body, approval(&joiner));
            assert_eq!(joined.devices, vec![device(1)]);
        }
    }

    #[test]
    fn a_joiner_refuses_an_approval_whose_signer_is_not_the_pinned_device() {
        let joiner = joiner();
        let signed = sign(&approval(&joiner), 1);

        let refused = verify_as_joiner(
            &signed,
            Some(&[device(2)]),
            &joiner,
            Some(cluster(5)),
            now(),
        );

        assert!(
            refused.is_err(),
            "a pin names who may approve, not the capsule"
        );
    }

    #[test]
    fn a_joiner_refuses_an_approval_that_does_not_name_it_in_every_field() {
        let joiner = joiner();
        let mismatch = AuthorityRefusal::Mismatch {
            field: "a joining Machine",
        };
        let renamed = machine("renamed", "10.210.1.0/24", 7);
        let moved = machine("joining", "10.210.2.0/24", 7);
        let rekeyed = machine("joining", "10.210.1.0/24", 8);

        let cases: Vec<(&str, JoinApproval, Option<SealingPublicKey>)> = vec![
            ("another allocation", approval(&renamed), Some(cluster(5))),
            ("another subnet", approval(&moved), Some(cluster(5))),
            (
                "another WireGuard key",
                approval(&rekeyed),
                Some(cluster(5)),
            ),
            (
                "another sealing recipient",
                JoinApproval {
                    sealing_recipient: WireGuardPublicKey([8; 32]),
                    ..approval(&joiner)
                },
                Some(cluster(5)),
            ),
            ("another Cluster", approval(&joiner), Some(cluster(6))),
            ("no opened Sealing Key", approval(&joiner), None),
            (
                "a network without its subnet",
                JoinApproval {
                    network: "10.99.0.0/16".parse().unwrap(),
                    ..approval(&joiner)
                },
                Some(cluster(5)),
            ),
        ];
        for (name, body, opened) in cases {
            let refused = verify_as_joiner(&sign(&body, 1), None, &joiner, opened, now());
            assert_eq!(refused, Err(mismatch.clone()), "{name}");
        }
    }

    #[test]
    fn a_joiner_refuses_an_approval_with_no_single_admitting_machine_or_past_its_expiry() {
        let joiner = joiner();
        let none = JoinApproval {
            admitting_machines: Vec::new(),
            ..approval(&joiner)
        };
        let two = JoinApproval {
            admitting_machines: vec![joiner.id, MachineId::parse("b".repeat(32)).unwrap()],
            ..approval(&joiner)
        };
        for body in [none, two] {
            assert_eq!(
                verify_as_joiner(&sign(&body, 1), None, &joiner, Some(cluster(5)), now()),
                Err(AuthorityRefusal::Mismatch {
                    field: "one admitting Machine"
                })
            );
        }

        let late = verify_as_joiner(
            &sign(&approval(&joiner), 1),
            None,
            &joiner,
            Some(cluster(5)),
            now().plus(60_000 + crate::authority::validity::CLOCK_SKEW_MILLIS + 1),
        );
        assert_eq!(late, Err(AuthorityRefusal::Expired));
    }

    #[test]
    fn a_joiner_trusts_the_approvals_devices_only_as_far_as_the_pin_names_them() {
        let joiner = joiner();
        let body = JoinApproval {
            devices: vec![device(1), device(3)],
            ..approval(&joiner)
        };
        let signed = sign(&body, 1);

        let trusted = |pinned: Option<&[DeviceKey]>| {
            verify_as_joiner(&signed, pinned, &joiner, Some(cluster(5)), now())
                .unwrap()
                .devices
        };

        assert_eq!(trusted(None), vec![device(1), device(3)], "no pin");
        assert_eq!(
            trusted(Some(&[device(1), device(2)])),
            vec![device(1)],
            "the pin leaves out Device 3 and the approval leaves out Device 2"
        );
    }

    #[test]
    fn a_joiner_refuses_an_approval_that_leaves_out_its_own_signer_as_approved_devices() {
        let joiner = joiner();
        let leaves_out_signer = JoinApproval {
            devices: vec![device(3)],
            ..approval(&joiner)
        };
        let signed = sign(&leaves_out_signer, 1);
        let approved_devices = AuthorityRefusal::Mismatch {
            field: "approved Devices",
        };

        assert_eq!(
            verify_as_joiner(
                &signed,
                Some(&[device(1)]),
                &joiner,
                Some(cluster(5)),
                now()
            ),
            Err(approved_devices)
        );
        assert!(
            matches!(
                verify_as_joiner(&signed, None, &joiner, Some(cluster(5)), now()),
                Err(AuthorityRefusal::UnknownKey { .. })
            ),
            "with no pin the signature already fails on a signer the approval does not list"
        );

        let pinned_elsewhere = JoinApproval {
            devices: vec![device(1), device(3)],
            ..approval(&joiner)
        };
        assert_eq!(
            verify_as_joiner(
                &sign(&pinned_elsewhere, 1),
                Some(&[device(1)]),
                &joiner,
                Some(cluster(5)),
                now()
            )
            .map(|joined| joined.devices),
            Ok(vec![device(1)])
        );
    }

    #[test]
    fn a_joiner_names_the_allocation_before_the_approved_devices() {
        let joiner = joiner();
        let both_wrong = JoinApproval {
            devices: vec![device(3)],
            ..approval(&joiner)
        };

        assert_eq!(
            verify_as_joiner(
                &sign(&both_wrong, 1),
                Some(&[device(1)]),
                &joiner,
                Some(cluster(6)),
                now()
            ),
            Err(AuthorityRefusal::Mismatch {
                field: "a joining Machine"
            })
        );
    }

    fn row_carrying(approved: &Machine, body: &JoinApproval, seed: u8) -> Machine {
        Machine {
            join_approval: Some(sign(body, seed)),
            ..approved.clone()
        }
    }

    #[test]
    fn a_row_is_admitted_only_by_an_approval_for_exactly_that_machine_in_this_cluster() {
        let joiner = joiner();
        let body = approval(&joiner);
        let trusted = || [device(1)];

        let admitted = row_carrying(&joiner, &body, 1);
        assert!(admits_row(&admitted, trusted(), Some(cluster(5))));
        // Endpoints and policy may change after the join; only identity, key and subnet bind.
        let moved_endpoint = Machine {
            advertised_endpoints: Vec::new(),
            ..admitted.clone()
        };
        assert!(admits_row(&moved_endpoint, trusted(), Some(cluster(5))));

        assert!(
            !admits_row(&joiner, trusted(), Some(cluster(5))),
            "no approval"
        );
        assert!(
            !admits_row(&admitted, [device(2)], Some(cluster(5))),
            "untrusted signer"
        );
        assert!(!admits_row(&admitted, [], Some(cluster(5))), "no Devices");
        assert!(
            !admits_row(&admitted, trusted(), Some(cluster(6))),
            "another Cluster"
        );
        assert!(
            !admits_row(&admitted, trusted(), None),
            "no Cluster known yet"
        );
        let signed_by_another = row_carrying(&joiner, &body, 2);
        assert!(
            !admits_row(&signed_by_another, trusted(), Some(cluster(5))),
            "signed by 2"
        );

        let other_subnet = machine("joining", "10.210.2.0/24", 7);
        let wrong_row = Machine {
            join_approval: admitted.join_approval.clone(),
            ..other_subnet
        };
        assert!(
            !admits_row(&wrong_row, trusted(), Some(cluster(5))),
            "another subnet"
        );
        let other_key = Machine {
            join_approval: admitted.join_approval,
            ..machine("joining", "10.210.1.0/24", 8)
        };
        assert!(
            !admits_row(&other_key, trusted(), Some(cluster(5))),
            "another key"
        );
    }

    #[test]
    fn join_code_exposes_one_hundred_bits_and_binds_both_identity_and_key() {
        let machine = MachineId::random();
        let key = WireGuardPublicKey([1; 32]);
        let code = code(machine, key);
        assert_eq!(code.len(), 20);
        assert!(
            code.bytes()
                .all(|byte| b"abcdefghijklmnopqrstuvwxyz234567".contains(&byte))
        );
        assert_ne!(code, super::code(MachineId::random(), key));
        assert_ne!(code, super::code(machine, WireGuardPublicKey([2; 32])));
    }

    #[test]
    fn join_code_matches_the_vector_clouds_copy_is_tested_against() {
        let machine = MachineId::parse("0123456789abcdef0123456789abcdef").unwrap();
        let key = WireGuardPublicKey(std::array::from_fn(|index| index as u8));
        assert_eq!(code(machine, key), "jhwpvddkhc7z72egn3lg");
    }

    #[test]
    fn a_join_confirmation_travels_as_lowercase_hex() {
        let confirmation = JoinConfirmation([0xab; 32]);
        let wire = serde_json::to_string(&confirmation).unwrap();
        assert_eq!(wire, format!("\"{}\"", "ab".repeat(32)));
        assert_eq!(
            serde_json::from_str::<JoinConfirmation>(&wire).unwrap(),
            confirmation
        );
        assert!(
            serde_json::from_str::<JoinConfirmation>(&format!("\"{}\"", "AB".repeat(32))).is_err()
        );
        assert!(serde_json::from_str::<JoinConfirmation>("\"abab\"").is_err());
    }
}
