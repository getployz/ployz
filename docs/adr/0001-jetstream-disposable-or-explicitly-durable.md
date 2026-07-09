# JetStream Stores Disposable Or Explicitly Durable Control-Plane State

Ployz should avoid making JetStream the only unrebuildable source for core operations. JetStream records should be classified as live observations, rebuildable secondary indexes, disposable operation memory, disposable job triggers, optional evidence/history, or explicitly named durable authority.

After JetStream loss, core recovery should be an explicit reindex operation: machines and data-plane roles reconnect or rejoin, publish fresh signed facts from Docker, Host Runner state, gateway/DNS last-known-good state, certificate authority material, and local role authority, then control rebuilds indexes and adopts only unambiguous state. Ambiguous or missing facts remain observations until a follow-up operation repairs them.

This is a deliberate trade-off against event-sourced workflow machinery, durable submit idempotency indexes, owner leases, and canonical JetStream backup assumptions. The goal is to keep JetStream useful as the NATS persistence surface without making every operation depend on perfect JetStream survival.
