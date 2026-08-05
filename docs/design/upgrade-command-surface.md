# The upgrade command surface

Design spec from the wayfinder ticket [Decide: the upgrade command
surface](https://github.com/getployz/ployz/issues/797). Names the operator
surface for the caller-paced binary swap that [Keeper's charter
(#784)](https://github.com/getployz/ployz/issues/784) decided: the unprivileged
API fold receives `{version, sha256, url}` over mesh HTTPS, stages and
hash-verifies the artifact at a content-addressed path, then asks root Keeper
over a local socket to flip the `current` symlink and restart keeper-first,
then the fold units. `OnFailure=` symlink revert is the product's only rollback
(a dead new keeper can't take the command that would fix it). This spec owns the
CLI verb, not that machinery.

Sits over the [status/doctor UX (#793)](cli-token-status-doctor-ux.md) — it
resolves that doc's `ployz <upgrade-command>` placeholder — and inherits the two
framing facts from it: the **CLI is a remote mesh peer** (every command is a
remote query answered by whichever machine it reaches), and **ids are
machinery** (select by name/hostname).

## The command

```
ployz machine upgrade <name>...        upgrade the named machines
ployz machine upgrade --all            upgrade every machine
ployz machine upgrade --outdated       upgrade every machine behind the target
```

Noun-verb, matching `machine join` / `machine rm` / `machine reset`. The
primitive is **per-machine** — the machine is the unit of work in the charter,
so it is the argument, never a flag on a cluster-scoped verb.

A bare `ployz machine upgrade` with no selector is a **typed refusal** that
names the three selectors. Upgrading every machine is a deliberate `--all`,
never an accidental default: the refusal performs no work, it names the command
that does.

`--outdated` targets every machine whose reported `machine_status` version is
behind the resolved target version. It is also the natural **resume** after a
halted `--all` run (see Pacing).

## Where the artifact comes from

The `{version, sha256, url}` triple the charter hands to the machine is resolved
by the CLI before it calls the machine. Three sources, most-trusted first:

```
ployz machine upgrade edge-b                        latest on the channel
ployz machine upgrade edge-b --version v0.1.0-alpha.7   pin a released version
ployz machine upgrade edge-b --url U --sha256 H     manual escape (both-or-neither)
```

- **Default — channel latest.** The CLI reads the release channel manifest
  (`https://ployz.sh/channels/<name>`, the incumbent's `release.env` shape, which
  carries artifact URL + sha256; #788 flips this channel to the coreless line on
  its first tag) and resolves the newest release's `url` + `sha256`.
- **`--version <v>` — pin.** Same channel lookup, an explicit released version
  instead of latest. This is also how you move a machine to an *older* release
  (rollback-by-redeploy through the trusted manifest); the charter's `OnFailure`
  revert is the crash-safety net, `--version` is the deliberate downgrade.
- **`--url --sha256` — manual escape, both-or-neither.** The fully explicit
  path for dev builds and airgapped/mirror artifacts. Passing one without the
  other is a typed refusal.

In every case the CLI **prints the resolved triple** (version + short sha256)
before acting, so the operator sees exactly what will be applied. In v2 the
artifact is a single `ployzd` binary (#790), so the triple describes one file.

The **URL is fetched by the target machine's API fold**, not the CLI — so a
`--url` must be reachable from the machine, and channel/mirror hosting is a
machine-reachability concern, not a CLI one.

## sha256 is a trust boundary

The hash is the only thing standing between a mesh-HTTPS-reachable URL and a
root symlink flip, so verification **fails closed** and does no work on mismatch.
It happens **on the machine**, in the unprivileged API fold, before Keeper is
ever contacted:

```
$ ployz machine upgrade edge-b
  edge-b  fetching v0.1.0-alpha.7 ... 14 MiB
  edge-b  sha256 MISMATCH
          expected f3a9...  got 8c1b...
  upgrade refused on edge-b — artifact not applied, machine unchanged
error: 1 machine failed verification
```

A mismatch or download failure is a **terminal refusal**: the staged file is
discarded, Keeper is never asked to swap, and the command returns a typed error
naming the machine and expected-vs-got (enough to tell a corrupted mirror from a
stale pin). The machine is byte-for-byte unchanged.

## The swap and its confirmation

The swap restarts the very API fold that answers the operator, so the command
cannot hold one open response across it. It is synchronous exactly where it
**can** fail cleanly, and hands off to existing surfaces for the rest.

**Phase 1 — synchronous, one connection.** The fold downloads, verifies sha256,
stages at the content-addressed path, and asks Keeper to arm the swap. Every
failure that has a clean, no-op shape (bad hash, download failure, disk floor,
Keeper refusal) lands here as a typed terminal error. The fold acks
`staged, swap armed` and only then triggers the keeper-first restart.

**Phase 2 — short best-effort confirm.** The connection drops (expected). The
CLI reconnects for a few seconds and reads the machine's `machine_status`
version row — the durable testimony Keeper already writes:

```
$ ployz machine upgrade edge-b
  edge-b  fetch + verify ok  (sha256 f3a9...)
  edge-b  staged, swap armed
  edge-b  now v0.1.0-alpha.7  ✓
```

If it does not come back on the target version within the short window:

```
  edge-b  swap armed; not yet confirming v0.1.0-alpha.7
          check:  ployz status edge-b
```

The CLI does **not** re-implement diagnosis. It owns the synchronous
trust-boundary work and a happy-path glance; every async question — did the
machine revert, is it unreachable, did the whole cluster converge — hands off to
`status` and `doctor`, the surfaces built for exactly that. This is the
product's "a refusal names the command that resolves it" law.

### Revert needs no CLI machinery

The charter's `OnFailure=` revert lives entirely in systemd unit config (#784):
a new keeper that never reaches first-converge trips it, and the machine comes
back **healthy on the previous binary**. The CLI needs no revert-detection code
for it. A reverted machine simply shows as version-skewed in `doctor`, which
already names `ployz machine upgrade <name>` as the repair — the same command
you would rerun. Self-healing UX, zero bespoke revert path.

## Pacing lives in the caller

Keeper never paces, sequences, or coordinates (#784). The CLI is one caller
(Cloud is the other, doing waves in a workflow); when the CLI upgrades many
machines, the caller owns the pacing policy.

`--all` / `--outdated` run **sequentially, halting on the first failure**:

```
$ ployz machine upgrade --all
  core-1  ✓ v0.1.0-alpha.7
  edge-a  ✓ v0.1.0-alpha.7
  edge-b  swap armed; not confirming v0.1.0-alpha.7 — stopping
error: halted after edge-b — 2 upgraded, edge-b unconfirmed, 1 not attempted (edge-c)
        resume:  ployz machine upgrade --outdated
```

- **One machine mid-swap at a time.** Each machine finishes its
  verify→arm→confirm before the next starts; never more than one machine in a
  swap window.
- **Stop on first failure.** A sha mismatch or an unconfirmed swap halts the
  run — never roll a bad release across the whole cluster. An operator who wants
  to plow ahead scripts per-machine names.
- **Resume is `--outdated`.** It re-targets whoever is still behind, so the
  fix after a halt is one obvious command.

## doctor resolution

This spec resolves the `ployz <upgrade-command>` placeholder in `doctor`
Finding 2 (skipped newer-`v` rows / mixed binary versions, #793). `doctor`
already sweeps every machine's `machine_status` version, finds the newest, and
names the lagging machines; the repair line names them explicitly:

```
$ ployz doctor
⚠ mixed binary versions in the cluster
    newest:  v0.1.0-alpha.7   (edge-a, core-1)
    behind:  v0.1.0-alpha.5   (edge-b — skipping v=2 rows in `services`)
  roll the lagging machines forward:  ployz machine upgrade edge-b
```

Multiple laggards are listed as arguments (`... upgrade edge-b edge-c`) —
copy-paste-ready, what-you-see-is-what-you-touch, matching the ids-are-machinery
rule. `doctor` Finding 3's repair is **not** owned here; it is the repair-kit
composition (`machine rm` + on-host `machine reset` + rejoin, #798).

## Rejected along the way

- **A held-open response across the swap** — the fold answering the operator is
  itself restarted; the socket cannot survive. Synchronous verify+arm, then
  reconnect-and-read the version row.
- **An SSE progress stream for the swap** — the SSE emitter is the fold being
  restarted; the stream dies at the same instant. Same reconnect problem, more
  machinery.
- **CLI-side revert detection (a 90s ceiling + three bespoke terminal
  outcomes)** — this re-implements `doctor`'s version sweep inside `upgrade`. A
  reverted machine shows as skew in the surface built for it; the upgrade
  command stays lean.
- **`--all` continuing past failures** — keeps pushing a binary that already
  failed once, the opposite of what a bad release needs. Halt, resume with
  `--outdated`.
- **A top-level `ployz upgrade` / cluster-scoped `ployz cluster upgrade`** — both
  fight the noun-verb grammar and the per-machine primitive; the machine is the
  argument, not a flag.
- **A `--force` upgrade variant** — there is no forced/unforced split that would
  commit identical truth; the artifact triple and sha gate are the only inputs.
