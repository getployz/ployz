//! Byte-for-byte pins of `restart-approvals.json`, a persisted file older daemons wrote and
//! newer ones must read and extend. A checked-in sample is loaded, one verb of every kind
//! runs on it, and the file is compared after each one: what it writes, what it writes
//! and then refuses, and what it leaves alone.
//!
//! `golden/sample.json` was written by main's daemon code from a log with one entry of
//! every section, and each `golden/<step>.json` is what that code wrote for the step.

use ployz_core::{
    DeviceKey, MachineId, MachineName, MachineSubnet, Namespace, SealedSealingKey,
    SealingPublicKey, WireGuardPublicKey,
    authority::{
        AutoDeployRule, BuildClaim, BuildStage, CommitAuthority, CommitSha, DeployApproval,
        ImageDigest, PolicyDigest, RuleId, Signed, Signer, SignerKey, StartingSet,
        join::{JoinAllocation, JoinApproval},
        rule::{AttestKey, Builder, RuleEvent, RuleSource, Runner},
        starting_set::Member,
    },
};

use super::*;

const SAMPLE: &[u8] = include_bytes!("golden/sample.json");

const T: u64 = 1_700_000_000_000;

fn at(offset: u64) -> Timestamp {
    Timestamp::from_millis(T + offset).unwrap()
}

fn operation(fill: char) -> OperationId {
    OperationId::parse(fill.to_string().repeat(32)).unwrap()
}

fn machine(fill: char) -> MachineId {
    MachineId::parse(fill.to_string().repeat(32)).unwrap()
}

fn container(fill: char) -> ContainerId {
    ContainerId::parse(fill.to_string().repeat(64)).unwrap()
}

fn slot(name: &str) -> ployz_core::CreationKey {
    ployz_core::CreationKey::try_from(name.to_owned()).unwrap()
}

fn signer() -> Signer {
    Signer::from_seed(&[1; 32])
}

fn device() -> DeviceKey {
    DeviceKey::from(signer().public())
}

fn restart(fill: char, proposal: Option<u32>) -> RestartApproval {
    RestartApproval {
        cluster: SealingPublicKey::from_bytes([1; 32]),
        operation: operation(fill),
        kind: ployz_core::authority::WorkflowKind::Restart,
        namespace: Namespace::parse("app").unwrap(),
        service: ServiceName::parse("api").unwrap(),
        machines: vec![],
        container: container('a'),
        issued_at: at(0),
        expires_at: at(600_000),
        signer: SignerKey::Device(device()),
        proposal: proposal.map(|index| ployz_core::authority::ProposalRef {
            operation: operation(fill),
            index,
        }),
    }
}

fn deploy(fill: char) -> DeployApproval {
    DeployApproval {
        cluster: SealingPublicKey::from_bytes([1; 32]),
        operation: operation(fill),
        namespace: Namespace::parse("app").unwrap(),
        machines: vec![],
        services: vec![],
        issued_at: at(0),
        expires_at: at(600_000),
        signer: SignerKey::Device(device()),
        proposal: None,
    }
}

fn starting_set(fill: char) -> StartingSet {
    StartingSet {
        operation: operation(fill),
        members: vec![Member {
            id: machine('5'),
            public_key: WireGuardPublicKey([5; 32]),
            subnet: "10.210.5.0/24".parse::<MachineSubnet>().unwrap(),
        }],
        issued_at: at(0),
        expires_at: at(600_000),
        signer: SignerKey::Device(device()),
        kept_keys: Vec::new(),
    }
}

fn joined(fill: char, reply: JoinReply) -> JoinResult {
    let id = machine(fill);
    let approval = JoinApproval {
        cluster: SealingPublicKey::from_bytes([1; 32]),
        machine: JoinAllocation {
            id,
            name: MachineName::parse(format!("joiner-{fill}")).unwrap(),
            subnet: "10.210.6.0/24".parse::<MachineSubnet>().unwrap(),
            public_key: WireGuardPublicKey([6; 32]),
            public_ip: None,
            advertised_endpoints: vec![],
            policy: ployz_core::InitialMachinePolicy {
                labels: BTreeMap::new(),
                accepts_builds: true,
                accepts_services: true,
                accepts_ingress: true,
            },
        },
        network: "10.210.0.0/16".parse().unwrap(),
        sealing_recipient: WireGuardPublicKey([6; 32]),
        devices: vec![device()],
        admitting_machines: vec![machine('9')],
        bootstrap: vec![],
        issued_at: at(0),
        expires_at: at(600_000),
        signer: device(),
    };
    JoinResult {
        approval: Signed::sign(&approval, &signer()).unwrap(),
        reply,
    }
}

fn rule(fill: u8, generation: u64, branch: &str) -> AutoDeployRule {
    AutoDeployRule {
        cluster: SealingPublicKey::from_bytes([1; 32]),
        rule: RuleId::from_bytes([fill; 16]),
        generation,
        namespace: Namespace::parse("app").unwrap(),
        machines: vec![machine('9')],
        source: RuleSource {
            repository_id: 1_450_985_696,
            owner_id: 354_323_584,
            repository: "Ployz-test/lab-service".to_owned(),
            branch: branch.to_owned(),
            workflow: ".github/workflows/ployz-build.yml".to_owned(),
            events: vec![RuleEvent::Push],
            builder: Builder {
                r#ref: "Ployz-test/build/.github/workflows/build.yml@1234567890abcdef1234567890abcdef12345678".to_owned(),
                sha: CommitSha::parse("1234567890abcdef1234567890abcdef12345678").unwrap(),
            },
            runner: Runner::GithubHosted,
        },
        services: vec![],
        issued_at: at(0),
        ended: false,
        signer: SignerKey::Device(device()),
    }
}

fn built(rule: &AutoDeployRule, commit: &str) -> BuildClaim {
    BuildClaim {
        cluster: rule.cluster,
        rule: rule.rule,
        generation: rule.generation,
        service: ServiceName::parse("web").unwrap(),
        policy: PolicyDigest::parse(&"cd".repeat(32)).unwrap(),
        commit: CommitSha::parse(commit).unwrap(),
        authority: CommitAuthority::Head,
        stage: BuildStage::Built {
            image: ImageDigest::parse(&format!("sha256:{}", "ef".repeat(32))).unwrap(),
            attest: AttestKey::from_bytes([7; 32]),
        },
    }
}

const COMMIT_A: &str = "54dfcf4a46870fa09f5d5d6c965dd705c1a24e5c";
const COMMIT_B: &str = "64dfcf4a46870fa09f5d5d6c965dd705c1a24e5c";

fn channel(channel: ployz_core::ReleaseChannel, generation: u64) -> UpgradeChannel {
    UpgradeChannel {
        cluster: SealingPublicKey::from_bytes([1; 32]),
        channel,
        generation,
        signer: SignerKey::Device(device()),
    }
}

fn volume() -> ployz_core::DockerVolumeName {
    ployz_core::DockerVolumeName::parse("app_data").unwrap()
}

/// The log over a directory holding `SAMPLE`, and the file as the last step left it: its
/// bytes, and its inode, which every write replaces and a step that keeps the file leaves.
struct Walk {
    dir: tempfile::TempDir,
    log: Arc<ConsumptionLog>,
    last: Vec<u8>,
    inode: u64,
}

impl Walk {
    fn start(sample: &[u8]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(FILE_NAME), sample).unwrap();
        let mut walk = Self {
            log: ConsumptionLog::new(dir.path()),
            last: sample.to_vec(),
            inode: 0,
            dir,
        };
        walk.inode = walk.inode();
        walk
    }

    fn inode(&self) -> u64 {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(self.dir.path().join(FILE_NAME))
            .unwrap()
            .ino()
    }

    fn bytes(&self) -> Vec<u8> {
        std::fs::read(self.dir.path().join(FILE_NAME)).unwrap()
    }

    /// The file now holds exactly `golden`, and changed.
    fn wrote(&mut self, name: &str, golden: &[u8]) {
        let bytes = self.bytes();
        assert_eq!(
            String::from_utf8_lossy(&bytes),
            String::from_utf8_lossy(golden),
            "{name}"
        );
        assert_ne!(bytes, self.last, "{name} wrote nothing");
        self.last = bytes;
        self.inode = self.inode();
    }

    /// The file is the bytes it was after the step before.
    fn kept(&self, name: &str) {
        assert_eq!(
            String::from_utf8_lossy(&self.bytes()),
            String::from_utf8_lossy(&self.last),
            "{name} wrote"
        );
        assert_eq!(self.inode(), self.inode, "{name} rewrote the file");
    }
}

macro_rules! golden {
    ($name:literal) => {
        include_bytes!(concat!("golden/", $name, ".json"))
    };
}

fn refused(result: Result<(), Error>) -> AuthorityRefusal {
    match result {
        Err(
            Error::RestartRefused(refusal)
            | Error::DeployRefused(refusal)
            | Error::EnforcementRefused(refusal)
            | Error::RuleGrantRefused(refusal),
        ) => refusal,
        other => panic!("not a refusal: {other:?}"),
    }
}

#[test]
fn restart_approvals_json_keeps_its_bytes_through_every_verb() {
    let mut walk = Walk::start(SAMPLE);
    let log = walk.log.clone();

    // A restart step is written before its caller acts.
    log.consume(
        &restart('a', None),
        &container('a'),
        RestartStep::Hold,
        at(1_000),
    )
    .unwrap();
    walk.wrote("restart", golden!("restart"));
    // A step a proposal's index names is a hashed entry of its own.
    log.consume(
        &restart('b', Some(2)),
        &container('a'),
        RestartStep::Start,
        at(2_000),
    )
    .unwrap();
    walk.wrote("restart-proposal", golden!("restart-proposal"));
    // A step already reached is written, the clock floor and the pruning with it, and
    // then refused.
    assert!(matches!(
        refused(log.consume(
            &restart('a', None),
            &container('a'),
            RestartStep::Hold,
            at(3_000)
        )),
        AuthorityRefusal::Replayed { .. }
    ));
    walk.wrote("restart-replayed", golden!("restart-replayed"));
    // An approval past its window is refused before anything is written.
    let late = RestartApproval {
        expires_at: at(100),
        ..restart('c', None)
    };
    assert!(matches!(
        refused(log.consume(&late, &container('a'), RestartStep::Hold, at(300_000))),
        AuthorityRefusal::Expired
    ));
    walk.kept("restart-expired");

    // A hook place is taken once, and a resend for the same slot passes untouched.
    let api = ServiceName::parse("api").unwrap();
    log.consume_hook(&deploy('d'), (&api, &slot("hook-1")), at(5_000))
        .unwrap();
    walk.wrote("hook", golden!("hook"));
    log.consume_hook(&deploy('d'), (&api, &slot("hook-1")), at(5_500))
        .unwrap();
    walk.kept("hook-resent");
    assert!(matches!(
        refused(log.consume_hook(&deploy('d'), (&api, &slot("hook-2")), at(6_000))),
        AuthorityRefusal::Replayed { .. }
    ));
    walk.kept("hook-other-slot");
    assert!(!log.hook_made("hook-1").unwrap());
    log.mark_hook_made("hook-1").unwrap();
    walk.wrote("hook-made", golden!("hook-made"));
    assert!(log.hook_made("hook-1").unwrap());
    log.mark_hook_made("hook-1").unwrap();
    log.mark_hook_made("no-such-slot").unwrap();
    walk.kept("hook-made-again");

    // A starting set is stored as an empty operation, and used once.
    log.consume_starting_set(&starting_set('e'), at(7_000))
        .unwrap();
    walk.wrote("starting-set", golden!("starting-set"));
    assert!(matches!(
        refused(log.consume_starting_set(&starting_set('e'), at(8_000))),
        AuthorityRefusal::Replayed { .. }
    ));
    walk.kept("starting-set-replayed");

    // A join is kept by its Machine and consumed once.
    assert!(log.join_result(machine('f')).unwrap().is_none());
    log.require_unused_join(machine('f')).unwrap();
    log.consume_join(
        joined(
            'f',
            JoinReply::Share(SealedSealingKey::from_bytes(vec![9; 8])),
        ),
        at(9_000),
    )
    .unwrap();
    walk.wrote("join", golden!("join"));
    assert!(log.join_result(machine('f')).unwrap().is_some());
    assert!(matches!(
        log.require_unused_join(machine('f')),
        Err(Error::JoinConsumed(_))
    ));
    assert!(matches!(
        log.consume_join(
            joined(
                'f',
                JoinReply::Share(SealedSealingKey::from_bytes(vec![9; 8]))
            ),
            at(9_500)
        ),
        Err(Error::JoinConsumed(_))
    ));
    walk.kept("join-consumed");

    // A GitHub token spends once, and raises its rule's mark.
    let second = rule(0x22, 1, "main");
    log.consume_rule_proof(
        &second,
        Error::RuleGrantRefused,
        "jti-1",
        at(3_000_000),
        at(10_000),
    )
    .unwrap();
    walk.wrote("rule-proof", golden!("rule-proof"));
    assert!(matches!(
        refused(log.consume_rule_proof(
            &second,
            Error::RuleGrantRefused,
            "jti-1",
            at(3_000_000),
            at(11_000)
        )),
        AuthorityRefusal::ProofReplayed
    ));
    walk.kept("rule-proof-replayed");
    assert!(matches!(
        refused(log.consume_rule_proof(
            &rule(0x22, 1, "other"),
            Error::RuleGrantRefused,
            "jti-2",
            at(3_000_000),
            at(11_500)
        )),
        AuthorityRefusal::Mismatch { .. }
    ));
    walk.kept("rule-proof-moved");

    // A rule's mark is written only when it rises.
    let third = rule(0x33, 5, "main");
    log.mark_rule(&third, Error::RuleGrantRefused).unwrap();
    walk.wrote("mark", golden!("mark"));
    log.mark_rule(&third, Error::RuleGrantRefused).unwrap();
    log.mark_rule(&rule(0x33, 5, "main"), Error::RuleGrantRefused)
        .unwrap();
    walk.kept("mark-same");
    assert!(matches!(
        refused(log.mark_rule(&rule(0x33, 4, "main"), Error::RuleGrantRefused)),
        AuthorityRefusal::Mismatch { .. }
    ));
    walk.kept("mark-lower");
    log.mark_rule(&rule(0x33, 6, "main"), Error::RuleGrantRefused)
        .unwrap();
    walk.wrote("mark-raised", golden!("mark-raised"));

    // The build a rule's Service last created from is kept, and only changes the file
    // when it differs.
    let fourth = rule(0x44, 1, "main");
    let claim = built(&fourth, COMMIT_A);
    assert!(!log.accepted(&fourth, &claim).unwrap());
    log.accept(&fourth, None, &claim, Error::RuleGrantRefused)
        .unwrap();
    walk.wrote("accept", golden!("accept"));
    assert!(log.accepted(&fourth, &claim).unwrap());
    log.accept(&fourth, None, &claim, Error::RuleGrantRefused)
        .unwrap();
    walk.kept("accept-same");
    let grant_stage = BuildClaim {
        stage: BuildStage::Grant {
            holder: ployz_core::BuildGrantId::parse(hex::encode([3u8; 32])).unwrap(),
            recipient: ployz_core::authority::RecipientKey::from_bytes([4; 32]),
        },
        ..claim.clone()
    };
    log.accept(&fourth, None, &grant_stage, Error::RuleGrantRefused)
        .unwrap();
    walk.kept("accept-grant-stage");
    assert!(matches!(
        refused(log.accept(
            &rule(0x33, 1, "main"),
            None,
            &claim,
            Error::RuleGrantRefused
        )),
        AuthorityRefusal::Mismatch { .. }
    ));
    walk.kept("accept-superseded");

    // A rule's push runs its hook once, and a resend for the same slot passes untouched.
    let pushed = built(&fourth, COMMIT_B);
    log.consume_rule_hook(
        (fourth.rule, None),
        &pushed,
        None,
        &slot("rule-hook-1"),
        Error::RuleGrantRefused,
    )
    .unwrap();
    walk.wrote("rule-hook", golden!("rule-hook"));
    log.consume_rule_hook(
        (fourth.rule, None),
        &pushed,
        None,
        &slot("rule-hook-1"),
        Error::RuleGrantRefused,
    )
    .unwrap();
    walk.kept("rule-hook-resent");
    assert!(matches!(
        refused(log.consume_rule_hook(
            (fourth.rule, None),
            &pushed,
            None,
            &slot("rule-hook-2"),
            Error::RuleGrantRefused,
        )),
        AuthorityRefusal::Mismatch { .. }
    ));
    walk.kept("rule-hook-other-slot");
    log.mark_hook_made("rule-hook-1").unwrap();
    walk.wrote("rule-hook-made", golden!("rule-hook-made"));

    // Volume records are written when a change alters them, and an answer is returned
    // either way.
    let changed = log
        .change_volumes(at(12_000), |records, _| {
            records.withdraw(&machine('9'), &volume(), &machine('8'));
            Ok(true)
        })
        .unwrap();
    assert!(changed);
    walk.wrote("volumes", golden!("volumes"));
    let unchanged = log
        .change_volumes(at(13_000), |records, _| {
            records.end(operation('7'));
            Ok(true)
        })
        .unwrap();
    assert!(unchanged);
    walk.kept("volumes-unchanged");
    assert!(matches!(
        log.change_volumes(at(14_000), |_, _| -> Result<(), Error> {
            Err(Error::NotParticipating)
        }),
        Err(Error::NotParticipating)
    ));
    walk.kept("volumes-refused");
    log.read_volumes(|records| assert_eq!(records.withdrawals().len(), 2))
        .unwrap();

    // A Release Channel's mark is written only when it rises, and a resend of the same
    // approval passes untouched.
    use ployz_core::ReleaseChannel::{Beta, Stable};
    log.mark_upgrade_channel(&channel(Beta, 2)).unwrap();
    walk.wrote("upgrade-channel", golden!("upgrade-channel"));
    log.mark_upgrade_channel(&channel(Beta, 2)).unwrap();
    walk.kept("upgrade-channel-resent");
    for stale in [channel(Stable, 1), channel(Stable, 2)] {
        assert!(matches!(
            log.mark_upgrade_channel(&stale),
            Err(Error::UpgradeChannelRefused(
                AuthorityRefusal::Mismatch { .. }
            ))
        ));
    }
    walk.kept("upgrade-channel-lower");

    // Every step above survives a reopening of the file.
    let reopened = ConsumptionLog::new(walk.dir.path());
    assert!(matches!(
        refused(reopened.consume(
            &restart('a', None),
            &container('a'),
            RestartStep::Hold,
            at(15_000)
        )),
        AuthorityRefusal::Replayed { .. }
    ));
    assert!(reopened.join_result(machine('f')).unwrap().is_some());
    assert!(matches!(
        reopened.mark_upgrade_channel(&channel(ployz_core::ReleaseChannel::Stable, 1)),
        Err(Error::UpgradeChannelRefused(_))
    ));
}

#[test]
fn activation_keeps_joins_a_kept_key_signed_and_drops_every_accepted_build() {
    let walk = Walk::start(SAMPLE);

    let dropped = walk.log.at_activation(&[device()]).unwrap();

    assert_eq!(
        dropped,
        AtActivation {
            joins: Vec::new(),
            accepted: 1,
            grants: 0,
        }
    );
    let written: serde_json::Value = serde_json::from_slice(&walk.bytes()).unwrap();
    assert!(written.get("accepted").is_none());
    assert!(
        written
            .get("joins")
            .unwrap()
            .get(machine('3').as_str())
            .is_some()
    );
}

#[test]
fn activation_drops_a_join_no_kept_key_signed() {
    let walk = Walk::start(SAMPLE);
    let other = DeviceKey::from(Signer::from_seed(&[2; 32]).public());

    let dropped = walk.log.at_activation(&[other]).unwrap();

    assert_eq!(dropped.joins, vec![machine('3')]);
    assert!(walk.log.join_result(machine('3')).unwrap().is_none());
}
