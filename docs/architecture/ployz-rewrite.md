---
title: Ployz Rewrite Shape
status: draft
created: 2026-05-23
---

# Ployz Rewrite Shape

This rewrite treats `crates/ployz` as the product center. `crates/polis` is not
part of the new design path. It can stay in the tree while Ployz stops importing
it, then be deleted when unused.

## Goal

Build Ployz around product operations that are easy to read and hard to misuse.
The code should expose that this is a distributed system with peer nodes,
replicated cluster memory, explicit authority, leases, and fallible runtime
mutation, without forcing product modules to manually handle substrate
bookkeeping.

## Layers

### Product Orchestration

Owns product behavior:

- machine membership and removal;
- deploy planning and apply;
- domain readiness;
- ACME issuance;
- serving activation;
- volume transfer;
- gateway and DNS behavior.

Product code should read as observe, decide, mutate, verify. It should not know
about p2panda stream ack policy, replay mechanics, operation envelopes, or
projection cursor storage.

### Pure Control

Owns local, testable control values:

- command context;
- authority and grants;
- leases and claim guards;
- external attempts for unobservable external systems such as ACME;
- typed resource identities;
- structured failure types.

This layer should be mostly pure Rust: small values, enums, transition methods,
and traits for ports that genuinely have multiple implementations.

### Cluster Memory

Owns replicated cluster state through a private p2panda Node worker:

- p2panda Node lifecycle;
- persistent SQLite store setup;
- topic naming;
- typed Ployz cluster events;
- materializers;
- projection snapshots;
- freshness and health;
- rejection evidence;
- author-key to principal/grant checks before projection effects.

This is an internal Ployz module, not a reusable framework. p2panda owns the
distributed log mechanics. Ployz owns product semantics.

## Boundary Rules

- Product modules may depend on pure control and cluster memory APIs.
- Product modules must not import p2panda directly.
- Cluster memory may depend on p2panda.
- Cluster memory must not hide authority decisions or product conflict rules.
- `crates/polis` is ignored for new work.
- No compatibility shim is required unless a concrete rollout needs one.

## First Proof

The first rewrite slice should prove machine membership end to end:

1. Define a typed machine membership event.
2. Publish it through the private p2panda Node worker.
3. Materialize it into a machine membership view.
4. Ack only after successful materialization.
5. Replay unacked events after restart.
6. Reject unauthorized authors before projection changes.
7. Make machine-add product code read through the new Ployz-local API.

The success criterion is code shape, not only behavior. If machine add is harder
to read than the current product module, the cluster-memory API is wrong.

## Deletion Gate

Delete `crates/polis` only after `crates/ployz` no longer imports it.

Until then, treat Polis as old code. Do not extend it to support the rewrite.
