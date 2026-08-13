# Token, Status, and Doctor UX

The CLI is human-first, scriptable, and names resources the same way operators
do. Canonical names are the durable identities; normal commands never expose a
second row ID or an `--id` disambiguator.

## Token commands

```text
ployz token create <name> [--ttl 24h]
ployz token list [--all]
ployz token revoke <name>
```

Token creation prints the secret-bearing join blob exactly once and shows
follow-up commands through a shell variable. The row is keyed by the supplied
name and stores only the secret digest plus public metadata. Reusing a live name
is an explicit conflict; revocation deletes that exact named row.

The join blob embeds the token name so verification is one exact row lookup and
a constant-time secret-digest comparison. The secret remains random because it
is a credential; the token's identity does not.

## Status

`ployz status` is a concise current-state view:

- cluster name and answering machine;
- Corrosion caught-up, syncing, or degraded evidence;
- readiness barrier state;
- accepted machines, transport addresses, and handshake freshness;
- copy-ready handoffs when operator action is needed.

Status never infers health from an arbitrary row count. It uses the accepted
roster and typed testimony, distinguishes missing testimony from stale
testimony, and names machines by their canonical names.

## Doctor

`ployz doctor` reports evidence that current readers skipped or could not safely
act on:

- malformed, foreign-cluster, provider-mismatched, or newer-version rows;
- machines behind the newest valid release;
- inert rows authored by peers or machines no longer in the accepted roster;
- current-machine-authored foreign rows that require fence/reset/rejoin.

There is no shadow-row category. Natural keys make duplicate-name rows
unrepresentable, so doctor does not print higher-ID cleanup commands. Repairs
select exact canonical names:

```text
ployz machine rm <machine>
ployz peer rm <peer>
ployz namespace rm <namespace>
ployz route rm <hostname>
```

Option-looking names are protected with `--` where the command grammar permits
them. Doctor remains diagnostic: it prints commands but never performs repairs
itself.

## Exit behavior

- Clean status/doctor output exits successfully.
- Doctor findings use a distinct non-success outcome while still printing the
  full report.
- An unreachable cluster is distinct from findings and names the connection or
  context repair.
