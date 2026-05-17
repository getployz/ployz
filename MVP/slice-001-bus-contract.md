---
title: Slice 001 Bus Contract Notes
status: active
date: 2026-05-17
plan: MVP/slice-001-bus-e2e-contract-plan.md
---

# Slice 001 Bus Contract Notes

## Product Behavior Expressed

This slice expresses the control-plane behavior behind future product commands:

- ask many nodes for capacity,
- submit deploy work to one scheduler from a queue group,
- distinguish no responders from timeout,
- reject unauthorized publish and response attempts,
- drain the bus so new work fails visibly.

The E2E scenario reads in product-ish terms: capacity responders, deploy
submission, scheduler queue, and drain. It does not require NATS server setup,
daemon handler wiring, Docker, or store assets to prove those semantics.

## Glue Hidden

The bus primitive hides:

- wildcard subject matching,
- request inbox creation,
- one-use reply permits,
- queue group selection,
- request-many aggregation,
- authorization checks before handler dispatch,
- typed failure variants for no responders, timeout, draining, and
  unauthorized response.

Feature code should be able to talk in subjects, handlers, replies, and typed
errors instead of rebuilding transport-specific routing and timeout machinery.

## Still Too Ceremonial

- The first implementation is in-process, so transport identity is only modeled
  through principals and reply permits. The iroh adapter slice will need to keep
  endpoint identity explicit without leaking transport details into feature
  handlers.
- `RequestTarget::Pattern` is deliberately explicit, but the caller still has
  to provide a concrete message subject for the delivered request. A later slice
  should decide whether that subject should be a required operation subject or
  derived from the target.
- The grant model is intentionally small. Bridge/import/export tests will force
  a richer shape.

## Next Simplification Target

After this slice passes review, the next slice should either:

- add iroh transport behind the same contract, or
- add a first authority/fact proof that uses the bus without changing its
  surface.

The choice should come from a fresh planning pass against
[MVP/overall-plan.md](overall-plan.md).

## Simplicity Review Gate

Future business logic should not need to know how request inboxes, queue-group
selection, response permits, grant rechecks, handler fanout, or drain waiting
work. It should see subjects, typed targets, handlers, replies, and structured
errors.

For follow-up review, judge this slice on:

- whether a deploy or machine feature can use the bus without writing routing
  plumbing,
- whether each concept has one obvious representation,
- whether the public API names match the business semantics,
- whether tests read like product behavior instead of implementation
  choreography,
- whether remaining complexity is isolated in the bus implementation rather
  than pushed to feature code.

## Review Fixes Applied

The first review pass found several issues that were worth fixing inside this
slice rather than deferring:

- authority is now bound to one `MemoryBus` instead of being a reusable grant
  token,
- `request_many` authorizes both the responder target and the delivered
  operation subject,
- queue grants include the queue name, not only the subject pattern,
- overlapping queue subscriptions deliver at most once per queue name,
- the E2E harness now proves successful publish fanout and in-flight drain
  waiting.

The remaining known performance work is intentionally left for the scale slice:
bus-wide executor bounds, shared payload storage, queue indexes, richer latency
metrics, and large logical-node stress gates.
