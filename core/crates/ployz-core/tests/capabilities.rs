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
        "ployz.rule.passkey.v1",
        "gated: the dashboard signs a rule with a passkey only when every enforcing Machine advertises it. An older daemon refuses the rule's envelope for its unknown `assertions` field, and its `\"passkey\"` signer, so a rule deploy there fails closed",
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
        "ployz.volume.size.set.v1",
        "gated: a caller checks the contract first and refuses the resize, naming the Server whose daemon lacks it",
    ),
    (
        "ployz.volume.size.shrink.v1",
        "gated: a caller checks the contract on every Machine holding a copy and refuses the shrink, naming the Server whose daemon lacks it",
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
