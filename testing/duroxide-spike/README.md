# Duroxide qualification

> **PROTOTYPE — do not merge.** This scratch crate asks whether one long-lived
> Duroxide orchestration can be a node-local FIFO for cluster commands while
> accepting growing history, local-history loss, and at-least-once activity
> delivery.

Run from the repository root:

```sh
cargo run --manifest-path testing/duroxide-spike/Cargo.toml
```

The run wipes `target/PROTOTYPE-duroxide-state`, which contains the real SQLite
database and external-effect witness used by the crash check.
