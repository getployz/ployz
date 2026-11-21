---
name: verify-member-removal
description: Model-check how each Member records a removed Server and keeps it out (Forget's local denial and its delete, deletion evidence from removed_machines, the accept filter and the pin commit, a resent Register, gossip and crashes) with the Stateright model in core/crates/ployz-verify/src/member_removal.rs. Use when a change touches LocalMachine::forget or publish_assignment in core/crates/ployzd/src/machine/local_machine.rs, refuse_removed or joining_key in core/crates/ployzd/src/machine/local_machine/join_approval.rs, ReplicatedStore::machines or record_removed in core/crates/ployzd/src/corrosion/store.rs, removed_machines in corrosion/store/crsql.rs, accepts_machine, removes_member or unlisted_members in core/crates/ployzd/src/machine/mod.rs, or remove_members and pin_machines in core/crates/ployzd/src/machine/store.rs.
---

# Verify member removal

The model in `core/crates/ployz-verify/src/member_removal.rs` explores a Server S removed from three Members. Each Member has its own Corrosion replica of S's row and its own record, which survives a crash. M1 admits joins; M3 joined before S's row reached it. Forget, gossip, each Member's observation (read, `record_removed`, filter, pin commit), a Register resent to M1, and a crash interleave freely. The model is a hand copy of the code; each `Action` names the function it copies.

The properties:

- **A Member that recorded S removed keeps it removed and never pins or accepts S again.**
- **No Member records S removed unless S was forgotten.** A row missing from a replica is not evidence.
- **A Member that read deletion evidence records it before its observation filters.**

## Run it

```
cd core && cargo nextest run -p ployz-verify -E 'test(member_removal)' --no-capture
```

About two seconds. Green is five passing tests:

- `a_removed_server_stays_out_on_every_member_that_recorded_it` checks every property and every `sometimes` on the code as it ships. A `sometimes` never reached fails it: the model lost that path, so a property passing beside it proves nothing.
- The other four each apply one `Mutation` and expect a property to fail, printing the path: the removed check dropped from filter and pin, the check made outside the pin commit, a Register that clears the denial, and removal recorded from absence. One going green means the guard no longer earns its keep, or the model lost the path that shows it.

## Read a counterexample

Each numbered row is one step in the code's words, then each Member's replica row (`Present(n)` or `Deleted(n)`, with cr-sqlite's causal length), whether it pins S, whether it holds S removed, and where its observation stands. Confirm every step against the function it names before calling it a bug. If a step does something the code cannot, fix the model and say so.

## Keep the model in step

When a change moves where the removed set is read or written, update the matching `Action` arm in the same PR, keep the properties as they are, and rerun. Out of scope: Servers other than S, the WireGuard peers an observation applies, Leave, and a Register at a Member other than M1.
