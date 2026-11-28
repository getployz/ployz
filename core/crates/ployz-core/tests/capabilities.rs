use ployz_core::CapabilityAdvertisement;

fn names(class: CapabilityAdvertisement) -> Vec<String> {
    class
        .capabilities()
        .map(|name| name.as_str().to_owned())
        .collect()
}

/// Capabilities every Machine in the field serves. Serving rotation cut off daemons
/// that predate serving routes. Raise it only when no Machine runs older.
const FIELD_FLOOR: &[&str] = &[
    "ployz.build.grant.inspect.v1",
    "ployz.build.grant.mint.v1",
    "ployz.build.v1",
    "ployz.certificates.policy.v1",
    "ployz.certificates.publish.v1",
    "ployz.container.ensure.v1",
    "ployz.container.exec.v1",
    "ployz.container.inspect.v1",
    "ployz.container.list.v1",
    "ployz.image.ingest.ensure.v1",
    "ployz.image.list.v1",
    "ployz.image.prune.v1",
    "ployz.image.pull-from-machine.v1",
    "ployz.logs.tail.v1",
    "ployz.machine.enroll.v1",
    "ployz.machine.forget.v1",
    "ployz.machine.inspect.v1",
    "ployz.machine.leave.v1",
    "ployz.machine.list.v1",
    "ployz.machine.register.v1",
    "ployz.machine.set-management-client.v1",
    "ployz.machine.storage-observation.v1",
    "ployz.machine.update.v1",
    "ployz.machine.upgrade.request.v1",
    "ployz.rpc.describe-contract.v1",
    "ployz.runtime.watch.v1",
    "ployz.serving.routes.v1",
    "ployz.storage.inspect.v1",
    "ployz.storage.prepare.v1",
    "ployz.volume.copy.floor.v1",
    "ployz.volume.copy.inspect.v1",
    "ployz.volume.copy.set.v1",
    "ployz.volume.list.v1",
    "ployz.volume.remove.v1",
    "ployz.volume.seed.v1",
];

/// Each capability above the floor, and what a caller does when a Machine lacks it.
const ABOVE_FLOOR: &[(&str, &str)] = &[
    (
        "ployz.build.free.v1",
        "gated: nothing sends a free Build yet; Cloud must send one only to a Server that advertises it, and an older daemon refuses the unknown call",
    ),
    (
        "ployz.container.free-receipt.v1",
        "gated: Cloud must attach a receipt only for a Server that advertises it. An older daemon ignores `free_receipt` and pulls the unsigned creation's image itself, which a free Build's image ID never names in a registry, so it fails closed",
    ),
    (
        "ployz.machine.join-trust.v1",
        "gated: a Server seals the keys and removals it holds only for a recipient whose row claims it, and a joiner that claims it refuses to serve without them, naming the admitting Server to upgrade",
    ),
    (
        "ployz.machine.join.confirm.v1",
        "gated: the join driver sends it only with restrictions the admitting Server sealed for a joiner that claims join-trust.v1, which is installed with it; without them it refuses and names the admitting Server to upgrade",
    ),
    (
        "ployz.machine.set-key.v1",
        "refused legibly: `ployz key add` calls it only over the Server's own socket, where it is installed with the daemon; an older daemon refuses the unknown call and nothing is trusted",
    ),
    (
        "ployz.network.select-endpoint.v1",
        "refused legibly: `server set --wg-endpoint` lists the Server as missed and still updates the target",
    ),
    (
        "ployz.machine.set-push-target.v1",
        "gated: the SDK checks the contract before calling",
    ),
    (
        "ployz.machine.key-step.v1",
        "falls back: `ployz device rm` calls set-device.v1 Remove on an older daemon, which can hold no passkey",
    ),
    (
        "ployz.machine.forget-versions.v1",
        "gated: nothing sends a forget yet; the client that does must send it only to a Machine that advertises it and list the rest as missed",
    ),
    (
        "ployz.container.signed-version.v1",
        "falls back: an older daemon ignores `versions` on EnsureContainer; one not enforcing creates as before, and `ployz deploy` refuses to sign for one that enforces and doesn't advertise this, naming it to upgrade",
    ),
    (
        "ployz.container.signed-version.proposal.v1",
        "gated: B5 must have Cloud send a deploy step of a proposal only to a Machine that advertises it, since nothing sends one yet; an older enforcing Machine refuses one, as it refuses any signed version it can't verify",
    ),
    (
        "ployz.container.deploy-rule.v1",
        "gated: Cloud attaches a rule proof only for a Machine that advertises it. An older daemon ignores `rule`, and under Device enforcement it refuses the Operator's creation for want of a deploy approval, so a rule deploy there fails closed",
    ),
    (
        "ployz.container.deploy-registry-rule.v1",
        "names the Server: the runner refuses a Deploy once, naming the Server, when it would place a Service a registry rule covers on a Machine that doesn't advertise it. An older daemon refuses the rule's registry source anyway, so the deploy fails closed",
    ),
    (
        "ployz.rule.passkey.v1",
        "gated: the dashboard signs a rule with a passkey only when every enforcing Machine advertises it. An older daemon refuses the rule's envelope for its unknown `assertions` field, and its `\"passkey\"` signer, so a rule deploy there fails closed",
    ),
    (
        "ployz.container.preview-rule.v1",
        "gated: nothing signs a preview rule yet; Cloud must deploy one only to a Server advertising it. An older daemon refuses the rule for its unknown `preview` field, so a preview deploy there fails closed",
    ),
    (
        "ployz.authority.scoped-keys.v1",
        "fails closed: an older daemon never stores a grant, so it refuses a CI or agent key's signature as an unknown key, refuses a Key step that trusts one for its unknown kind, and an older joiner refuses a join approval carrying grants for its unknown field. A client that signs with a CI or agent key checks for it",
    ),
    (
        "ployz.authority.parent-versions.v1",
        "fails closed: an older daemon refuses a signed version naming a parent for its unknown field, so a client that signs one checks for it on every Server first",
    ),
    (
        "ployz.authority.key-prior.v1",
        "fails closed: an older daemon refuses a Key step carrying `prior` or `name` for its unknown fields, and a `remove_grant` for its unknown kind, so `ployz key` checks for it on every Server first",
    ),
    (
        "ployz.container.replaces.v1",
        "gated: a Deploy asks to remove a leftover copy's anonymous Volumes only on a Server advertising it, and asks any other to keep them, since an older daemon removes them as asked. An older daemon ignores `replaces` on EnsureContainer",
    ),
    (
        "ployz.volume.kept.list.v1",
        "refused legibly: `ployz server volumes` says the Server needs a daemon upgrade, and `ployz server show` leaves its kept volumes out",
    ),
    (
        "ployz.volume.kept.remove.v1",
        "refused legibly: `ployz server volumes prune` lists first, so an older daemon refuses the list and the CLI says the Server needs a daemon upgrade",
    ),
    (
        "ployz.metrics.live.v1",
        "refused legibly: `ployz metrics` names a Server whose daemon lacks it as not answering",
    ),
    (
        "ployz.container.ensure-pinned.v1",
        "gated: a drain sends a tag spec only to a Server whose row doesn't lack it, and says to upgrade the rest; an unclaimed older daemon refuses it as unsupported, which the drain reports the same way, with nothing started",
    ),
    (
        "ployz.image.pull.v1",
        "falls back: an older daemon pulls when it creates the Container",
    ),
    (
        "ployz.image.pull-free.v1",
        "falls back: an older daemon answers unsupported, and the client sends ployz.image.pull.v1",
    ),
    (
        "ployz.ingress.tunnel.v1",
        "gated: `ployz ingress use tunnel` checks each Machine's contract first and names a Server whose daemon lacks it as needing an upgrade; an older daemon refuses the Tunnel Door as unsupported",
    ),
    (
        "ployz.private-access.approval.v1",
        "falls back: an older daemon ignores `approval` on SetPrivateAccess; one not enforcing turns Private Access on as before, and one enforcing refuses an Operator's SetPrivateAccess as it did, naming the route",
    ),
    (
        "ployz.volume.trash.approval.v1",
        "gated: Ployz Cloud shows a Server's Trash only where it advertises it, and says to upgrade the Server elsewhere; `ployz volume trash` reaches the Server's own socket as before",
    ),
    (
        "ployz.volume.size.set.v1",
        "gated: a caller checks the contract first and refuses the resize, naming the Server whose daemon lacks it",
    ),
    (
        "ployz.volume.size.shrink.v1",
        "gated: a caller checks the contract on every Machine holding a copy and refuses the shrink, naming the Server whose daemon lacks it",
    ),
    (
        "ployz.preview.home.v1",
        "gated: nothing places a preview yet; Cloud must place one only on a Server advertising it. An older daemon admits a preview Namespace as an ordinary one",
    ),
    (
        "ployz.volume.start.v1",
        "gated: nothing sends an Operator's Create with versions yet; Cloud must send one only to a Server advertising it. An older daemon ignores `versions`, `rule` and `check` and refuses an Operator's Create before any copy changes, so a Move there fails closed",
    ),
    (
        "ployz.volume.start.image.v1",
        "gated: Cloud starts a Move's Writer only on a destination advertising it, and otherwise refuses naming the Server to upgrade. An older daemon pulls the start's image again on a start resent after its promote, so the image could have moved",
    ),
    (
        "ployz.volume.cut.running.v1",
        "gated: a drain sends a cut that needs its holder running only to a Server advertising it, and otherwise leaves the Service where it is, naming the Server to upgrade. An older daemon ignores `running` and cuts a stopped holder",
    ),
    (
        "ployz.volume.keep-written.v1",
        "gated: Cloud sends a full sync's Forget, or a mirror onto a slot a departure parked, only to a Server advertising it, and otherwise refuses naming the Server to upgrade. An older daemon ignores `discard` and destroys any slot a Forget names, as `mirror rm` asks",
    ),
    (
        "ployz.volume.seed-copy.v1",
        "gated: nothing sends it yet; Cloud must offer a restore into a new Volume only when every Member advertises it. An older daemon refuses the unknown call before any copy starts",
    ),
    (
        "ployz.volume.origin.v1",
        "refused legibly: a Server sends a born Volume's data only to a pull that names it, and the refusal names the capability; Legacy data moves between any two daemons as before",
    ),
    (
        "ployz.container.limits.update.v1",
        "gated: the planner updates in place only on a Machine advertising it, and replaces the Container on any other",
    ),
    (
        "ployz.container.placement.update.v1",
        "gated: the planner keeps a Container under a new placement only on a Machine advertising it, and replaces the Container on any other",
    ),
    (
        "ployz.serving.routed-addresses.v1",
        "claims nothing: status names no stale address for a Server whose daemon refuses it",
    ),
    (
        "ployz.ingress.doors.v1",
        "gated: the CLI calls SetIngressDoors only on Machines advertising it, falls back: an older daemon keeps its running Ingress Proxy and the CLI names the Server as needing a newer Ployz, claims nothing: no proxy change on that Machine",
    ),
    (
        "ployz.cluster.name.read.v1",
        "refused legibly: `ployz cluster name` names the Server whose daemon lacks it",
    ),
    (
        "ployz.cluster.name.v1",
        "gated: `ployz cluster name <name>` and the SDK check the contract first and refuse, naming the Server whose daemon lacks it",
    ),
    (
        "ployz.cluster.doors.v1",
        "falls back: `ployz ingress use`, `server init --ingress-closed` and Cloud founding declare through an online Server that serves it when the entry can't, and warn and set each Server's Doors as before when none can; claims nothing: the snapshot carries no declaration and readers infer the Doors from what runs, and an older Register ignores the joiner's claim",
    ),
    (
        "ployz.private-access.v1",
        "gated: `ployz private-access on` and `off` check each Machine's contract first and name a Server whose daemon lacks it as needing an upgrade, so that Machine runs no router",
    ),
    (
        "ployz.management.scope.v1",
        "gated: an older daemon serves an Operator caller as Full, so a path that sends a Sealed value first checks every Machine in the Cluster advertises it, and refuses otherwise",
    ),
    (
        "ployz.machine.upgrade.exact.v1",
        "claims nothing: Ployz Cloud names only a Release Channel, and an older daemon still takes any exact version from an Operator",
    ),
    (
        "ployz.env.sealed.v1",
        "gated: a path that sends a Sealed value first checks every Machine in the Cluster advertises it, and refuses naming each Machine that lacks it",
    ),
    (
        "ployz.build.sealed.v1",
        "gated: Builder selection for a Build that reads a Sealed value considers only Machines advertising it, and refuses naming each Machine that lacks it",
    ),
    (
        "ployz.build.grant.holder.v1",
        "gated: a GitHub check-in mints for the runner's key only on a Machine advertising it; otherwise it mints the old grant, and a Build that reads a Sealed value goes to its next Builder",
    ),
    (
        "ployz.build.grant.rule.v1",
        "gated: a rule build's grant request carries `rule` only to a Machine advertising it. An older daemon ignores `rule`, and under Device enforcement it refuses the Operator's mint, so a rule build there fails closed",
    ),
    (
        "ployz.sealing.generate.v1",
        "gated: the Sealing Key backfill reads each Machine's contract first and never picks a generator that lacks it",
    ),
    (
        "ployz.sealing.share.v1",
        "gated: the backfill seals from a holder advertising it, and reports a Cluster whose holders all lack it",
    ),
    (
        "ployz.sealing.install.v1",
        "gated: the backfill skips a Machine that lacks it and reports it as skipped",
    ),
    (
        "ployz.machine.enroll-for-recovery.v1",
        "gated: founding for recovery reads the new daemon's contract first and refuses one that lacks it",
    ),
    (
        "ployz.sealing.wrap.v1",
        "gated: `ployz recovery rotate` reads the contract first and names a Server that lacks it",
    ),
    (
        "ployz.sealing.recover.v1",
        "gated: `ployz recover` reads the contract first and names a Server that lacks it",
    ),
    (
        "ployz.machine.set-device.v1",
        "gated: `ployz device` checks each Machine's contract first, names a Server whose daemon lacks it as missing and exits partial",
    ),
    (
        "ployz.machine.join.pending.v1",
        "refused legibly: `ployz device approve --join` stops at a Server whose daemon lacks it, and the refusal names the capability",
    ),
    (
        "ployz.machine.join.approve.v1",
        "unreached: ApproveJoin goes only to the Machine that listed the pending join, which has it",
    ),
    (
        "ployz.preview.list.v1",
        "gated: Cloud lists a Server's previews only to close those it no longer holds open, on a Server advertising Close. An older daemon refuses the route, naming the capability",
    ),
    (
        "ployz.preview.close.v1",
        "gated: nothing closes a preview yet; Cloud must close one only on a Server advertising it. An older daemon refuses the route, naming the capability",
    ),
    (
        "ployz.volume.trash.v1",
        "gated: Ployz Cloud lists a Server's Trash only where it advertises ployz.volume.trash.approval.v1; an older daemon refuses an Operator's list, naming the capability",
    ),
    (
        "ployz.volume.trash-restore.v1",
        "gated: Ployz Cloud asks for a passkey and sends it only to a Server advertising ployz.volume.trash.approval.v1; an older daemon refuses an Operator's call as before, naming the capability",
    ),
    (
        "ployz.volume.trash-empty.v1",
        "gated: Ployz Cloud asks for a passkey and sends it only to a Server advertising ployz.volume.trash.approval.v1; an older daemon refuses an Operator's call as before, naming the capability",
    ),
    (
        "ployz.machine.join.device-approval.v1",
        "falls back: an older daemon never refuses Register with it, so the join proceeds unsigned as before",
    ),
];

/// A new RPC reaches Machines that predate it. Before adding a catalog row, decide
/// what callers do when a Machine refuses it, and record that in `ABOVE_FLOOR`.
#[test]
fn every_capability_above_the_field_floor_says_how_older_machines_are_handled() {
    let mut catalogued: Vec<String> = [
        CapabilityAdvertisement::Always,
        CapabilityAdvertisement::Container,
        CapabilityAdvertisement::Cluster,
    ]
    .into_iter()
    .flat_map(names)
    .collect();
    catalogued.sort();
    let mut known: Vec<&str> = FIELD_FLOOR
        .iter()
        .copied()
        .chain(ABOVE_FLOOR.iter().map(|(name, _)| *name))
        .collect();
    known.sort_unstable();
    let unhandled: Vec<&String> = catalogued
        .iter()
        .filter(|name| !known.contains(&name.as_str()))
        .collect();
    assert!(
        unhandled.is_empty(),
        "{unhandled:?} reach Machines older than the floor. Gate the call on the contract, \
         fall back, or let the refusal name the Server, then add a row to ABOVE_FLOOR"
    );
    let stale: Vec<&&str> = known
        .iter()
        .filter(|name| !catalogued.iter().any(|c| c == **name))
        .collect();
    assert!(stale.is_empty(), "{stale:?} are no longer catalogued");
}

mod coverage {
    use std::collections::BTreeSet;

    use ployz_core::ManagementScope;
    use ployz_core::authority::coverage::{AUTHORIZATION, Authorization, CASES, Expected};

    fn routes() -> Vec<(&'static str, ManagementScope)> {
        ployz_core::rpc::ROUTES
            .iter()
            .map(|route| (route.capability, route.scope))
            .collect()
    }

    fn catalogued(name: &str) -> bool {
        ployz_core::served_capabilities().contains(name)
    }

    #[test]
    fn every_route_has_one_authorization_class() {
        let routes = routes();
        for (capability, _) in &routes {
            let classes = AUTHORIZATION
                .iter()
                .filter(|(name, _)| name == capability)
                .count();
            assert_eq!(
                classes, 1,
                "{capability} has {classes} authorization classes, not one"
            );
        }
        for (name, _) in AUTHORIZATION {
            assert!(
                routes.iter().any(|(capability, _)| capability == name),
                "{name} is classed but is not a catalog route"
            );
        }
    }

    #[test]
    fn full_class_matches_catalog_scope() {
        for (capability, scope) in routes() {
            let class = ployz_core::authority::coverage::authorization(capability)
                .unwrap_or_else(|| panic!("{capability} has no class"));
            assert_eq!(
                class == Authorization::Full,
                scope == ManagementScope::Full,
                "{capability} is classed {class:?} but its catalog scope is {scope:?}"
            );
        }
    }

    /// The branches the security plan's live matrix lists for a route with several
    /// mutations, as they exist at this slice. A later slice that gives a route a new
    /// branch adds it here and to `CASES`.
    const BRANCHES: &[(&str, &[&str])] = &[
        (
            "ployz.machine.enroll.v1",
            &["member, no capsule", "member, approved capsule", "founder"],
        ),
        (
            "ployz.container.ensure.v1",
            &[
                "create, reaches data",
                "create, reaches no data",
                "start",
                "stop",
                "held",
                "out",
                "remove",
            ],
        ),
        (
            "ployz.machine.upgrade.request.v1",
            &[
                "a release channel",
                "an exact version no channel resolves to",
                "a downgrade",
            ],
        ),
        ("ployz.private-access.v1", &["on", "off"]),
        (
            "ployz.sealing.generate.v1",
            &[
                "the Cluster publishes no key",
                "the Cluster already publishes one",
            ],
        ),
        (
            "ployz.sealing.install.v1",
            &[
                "sealed by an accepted Machine's key",
                "sealed by any other key",
            ],
        ),
        (
            "ployz.machine.set-management-client.v1",
            &[
                "from Cloud's own connection",
                "from the mesh, or naming a Device slot",
            ],
        ),
        (
            "ployz.volume.copy.set.v1",
            &[
                "a Writer want with a start",
                "a pull",
                "a cut",
                "a promote",
                "a forget of a copy",
                "a forget of nothing",
                "floor, snapshot",
            ],
        ),
        (
            "ployz.build.grant.mint.v1",
            &["with a rule and its proof", "without"],
        ),
    ];

    #[test]
    fn every_signed_and_bound_route_has_its_cases() {
        let mut seen = BTreeSet::new();
        for case in CASES {
            let class = ployz_core::authority::coverage::authorization(case.capability)
                .unwrap_or_else(|| panic!("case {case:?} names no catalog route"));
            assert!(
                seen.insert((case.capability, case.branch)),
                "{} has two cases for branch {:?}",
                case.capability,
                case.branch
            );
            assert_ne!(
                class,
                Authorization::Read,
                "{} is a read and needs no case",
                case.capability
            );
            if let Expected::Refused(names) = case.expected {
                assert!(!names.is_empty(), "{case:?} names no capability");
                for name in names {
                    assert!(catalogued(name), "{case:?} names uncatalogued {name}");
                }
                if let Authorization::Signed(statements) = class {
                    for name in names {
                        assert!(
                            *name == case.capability
                                || statements
                                    .iter()
                                    .any(|statement| statement.capabilities().contains(name)),
                            "{case:?} names {name}, which neither the route nor its statements are"
                        );
                    }
                }
            }
            if class == Authorization::Full || matches!(class, Authorization::Signed(&[])) {
                assert!(
                    matches!(case.expected, Expected::Refused(_)),
                    "{case:?} must be refused: no statement admits an Operator"
                );
            }
        }
        for (capability, class) in AUTHORIZATION {
            if *class != Authorization::Read {
                assert!(
                    CASES.iter().any(|case| case.capability == *capability),
                    "{capability} is classed {class:?} and has no case"
                );
            }
        }
        for (capability, branches) in BRANCHES {
            for branch in *branches {
                assert!(
                    seen.contains(&(*capability, *branch)),
                    "{capability} has no case for branch {branch:?}"
                );
            }
        }
    }
}
