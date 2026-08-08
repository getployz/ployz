# `ployz init`: What Machine One Mints

Design spec from the wayfinder ticket [Decide: what ployz init mints on
machine one](https://github.com/getployz/ployz/issues/800). Sits over
[the row model](corrosion-row-model.md) (#785), [the mesh-provider +
Principal spec](mesh-provider-and-principal.md) (#787), [the crate
topology](binary-crate-topology.md) (#790), and the
[token/status/doctor UX](cli-token-status-doctor-ux.md) (#793).

## Two drivers, one primitive

Init is the single bootstrap exception to "the CLI is a remote mesh
peer": no mesh exists yet, and the driver is not a peer yet. One
on-host founding primitive does all the work; two drivers feed it the
same parameter set and are enrolled as the first `peers` row over
their own channel. Founder-vs-joiner is never a question a human
answers: the CLI driver founds explicitly, Cloud's brain decides it.

```
        ┌─────────── the shared bootstrap question set ───────────┐
        │        (flags on the CLI, form fields on Cloud)          │
        └────────────┬────────────────────────────┬───────────────┘
             CLI driver                     Cloud driver
             ployz init root@<ip>           dashboard form → answers
               ssh: fetch binary,             baked into the paste line
               run the primitive,             as ordinary flags; host
               enroll LAPTOP as peer          phones home only to report;
               over the ssh channel           CLOUD enrolled as peer
                     └────────────┬───────────────┘
                                  ▼
                  sudo ployz init   (on-host founding primitive)
```

Cloud's dashboard form answers are baked into the paste line as the
same flags the CLI passes — the on-host primitive has one parameter
surface, and the phone-home callback carries only enrollment and
progress reporting (Cloud's pubkey rides the `--cloud-token` value;
the host reports its identity back), never answers to fetch.

Both journeys end identically: the driver has mesh access, every
subsequent machine goes through the one token door (#787/#793). No
first token is minted at init — `ployz token create` is the next
primitive, over the mesh, when machine two arrives.

## The binary owns install and control

`ployz.sh` fetches the `ployz` binary for the host's arch. Nothing
else — the mutable channel stays Bootstrap Delivery convenience.
Install and control logic lives in the binary, backed by
`ployz-host-runner`'s privileged effects: `sudo ployz init` stages
`ployzd` and the exact-pinned Corrosion from the release channel,
invokes Docker's own official install path when Docker is missing
(Ployz never redistributes Docker), then runs the mint list. Three
spellings, one line:

```
on host:      curl -fsSL https://ployz.sh | sh && sudo ployz init [flags]
from laptop:  ployz init root@<ip>          (ssh runs exactly that line)
Cloud paste:  curl -fsSL https://ployz.sh | sh && sudo ployz init --cloud-token …
```

`ployzd` keeps only its role subcommands. The Cloud paste line keeps
today's shape with `host bootstrap cloud` collapsing into `init`.

## The questions: opt-ins that are hard to change later

A parameter becomes a question only if changing it later is expensive;
everything else is a flag with a default. Every question has a safe
default and a stated rationale, is enter-through-able on a TTY,
answerable by flags for CI/cloud-init, and rendered identically as
Cloud form fields. Bare `ployz init root@<ip>` founds a working
cluster.

```
$ ployz init root@203.0.113.7
  machine ares · cluster ares (rename later)

? volume storage      [zfs — pool "data" detected]      (no pool → [plain])
    zfs    per-volume datasets: quotas, snapshots; moving later = data migration
    plain  plain directories — not eligible for volume backups, instant
           migrations, or zero-downtime volume moves

? container network   [10.210.0.0/16]
    each machine takes a /24 (~250 machines); must not collide with your LAN;
    changing later means renumbering every machine

? service URLs        [ployz — instant *.<cluster>.ployz.app]
    ployz     managed wildcard; deploys get HTTPS URLs immediately
    custom    your domain — point *.suffix at the cluster, certs via ACME
    disabled  routes only when you attach them explicitly
```

- **Service URLs** is the incumbent `AutomaticHostnameConfiguration`
  enum (`Disabled | Ployz | Custom{suffix}`), asked at init instead of
  buried.
- **Mesh provider** joins the wizard only when Tailscale ships — a
  question whose second answer does not work cannot ship. Until then
  builtin is silent, and no `--mesh` flag exists either: a flag with
  one legal value is dead surface, and the enum in code proves the
  seam. Question and flag arrive together, additively.
- Flag-only, never asked: cluster name (default: machine-one hostname;
  rename later), machine name (hostname), WG endpoint (auto-detect) and
  port. ACME CA/contact is not init surface; it has a default and can be
  changed later. Constants: `.internal`
  (non-Ployz names forward upstream, so GCP's `.internal` still
  resolves), gateway ports, the derived ULA plane.

## Carry-forward: cluster-fixed vs machine defaults

Prefix, service URLs, and mesh are **cluster-fixed** — one answer,
every machine, recorded on the `cluster` row. Storage is a **machine
default**: machine one's answer becomes `storage_default` on the
cluster row, and every machine (machine one included) resolves its own
mode at admission:

1. explicit flag on the join/init line, else
2. eligibility gate: ZFS only when an imported pool exists **and**
   RAM ≥ 2 GB (below that, ARC pressure starves containers) — creating
   pools is the operator's job, never init's, else
3. the cluster default.

`storage_default` exists for exactly one case the gate cannot express:
the operator who wants **plain everywhere despite pools** (backups
handled elsewhere, ZFS deliberately unused). One init answer covers
every future join; without it, that operator flags every join line
forever. This was weighed against pure flag-else-gate and chosen
deliberately.

Joins stay unattended — joiners never prompt; only an explicit flag
overrides the gate. The resolved mode and its reason
(`default | flag | ineligible(low_ram)`) are recorded on the machines
row at admission — a durable fact placement and `doctor` can read, not
inferred liveness. The wizard's forfeit line is one shared string
constant, shown wherever plain is chosen or diagnosed (wizard, join
output, `doctor`); `machine ls` just shows `storage: plain` in its
column.

## Re-run: resume, no-op, refuse

The persisted `cluster_id` file is the distinguishing fact — no
heuristics. Four arrival states:

| state on the host | init does |
|---|---|
| clean machine | found the cluster |
| partial state, same `cluster_id` | **resume** — every step is check-then-do |
| complete state, same `cluster_id` | **no-op success** — print the summary |
| foreign state (another cluster, or joined elsewhere) | **typed refusal** naming `ployz machine reset` |

- Persisting the minted `cluster_id` is init's first durable act, so a
  re-run adopts it instead of minting a second cluster. This makes
  Cloud's retry loop and flaky-SSH re-runs safe with zero special
  casing.
- **`ployz machine reset`** is the teardown primitive `doctor`'s
  re-init placeholder was waiting for: stop units and wipe control-plane
  state and the Corrosion DB, while preserving workload-volume storage under
  `/var/lib/ployz`, Docker, and workload images. Init never resets anything
  — no `--force`, no auto-wipe under any flag.
- The refusal is refusal everywhere: it prints the exact reset
  command to copy-paste, and Cloud surfaces it as a failed session
  state with the same named repair. No driver-side consent prompt
  (rejected below).
- **`machine join` adopts the same four arrival states** (#798):
  clean → join, partial same-cluster → resume, complete
  same-cluster → no-op, foreign → typed refusal naming
  `ployz machine reset`. The door refusing foreign state at
  admission is what keeps a mesh member authoring
  foreign-`cluster_id` rows (`doctor` Finding 3) a rare disaster
  instead of a workflow.

## The repair kit: no re-init, no reseed

Decided in wayfinder
[#798](https://github.com/getployz/ployz/issues/798): "re-init"
and "reseed" are not commands. Every broken-machine and
broken-cluster state is repaired by a composition of existing
primitives, and the finding that diagnoses the state names the
subset that applies, in order:

```
              ┌────────────┬───────────────┬────────────────┐
              │ machine rm │ machine reset │ machine join   │
              │ (laptop,   │ (on host,     │ (on host,      │
              │  over mesh)│  sudo)        │  fresh token)  │
 ─────────────┼────────────┼───────────────┼────────────────┤
 foreign      │ ✓ fence    │ ✓ wipe bad    │ ✓ fresh        │
 identity     │   first    │   state       │   identity     │
 ─────────────┼────────────┼───────────────┼────────────────┤
 wiped disk   │ ✓ clear    │ — disaster    │ ✓ fresh        │
 (no roster)  │   corpse   │   already did │   identity     │
 ─────────────┼────────────┼───────────────┼────────────────┤
 fresh host   │     —      │      —        │ ✓              │
              └────────────┴───────────────┴────────────────┘
```

- `machine rm` comes **first**: the removal fence stops a
  misconfigured member's writes at the source, and clearing a
  wiped machine's corpse row frees its name before the rejoin
  (the corpse holds the lower ULID and would otherwise win the
  name collision).
- A wiped machine's old WG key is unrecoverable, so there is no
  rejoin primitive and no identity resurrection — join is the only
  door into any cluster, and a rejoining machine is a new machine.
- `machine reset` has no SSH driver form: the repair is an on-host
  line, which is also the only shape Cloud can relay (Cloud has no
  SSH path to customer machines). Cloud composes its own single
  paste line — `sudo ployz machine reset && sudo ployz machine
  join pzjoin_…` — from the same primitives; paste lines may fuse,
  commands may not.

## Refound: the cluster-scope escape hatch

Reseed is deleted from v2 — no command, no seed/fence field, no
dump format. Corrosion binary upgrades (until the rolling drill
certifies adjacent versions), destructive schema changes,
catastrophic store repair, compaction, and stray-row cleanup all
collapse to the **refound** composition — #788's "teardown + fresh
install + re-declared intent" applied to flag days. A runbook
page, not a command:

```
 every host:   sudo ployz machine reset
 machine one:  ployz init root@<ip>
 laptop:       ployz token create → paste the join line on each host
 laptop:       ployz namespace create …; deploy each service
```

Through it, containers keep serving — reset leaves Docker,
volumes, and images alone, and bids re-pin volumes to their
holders (#804) — but public ingress and internal DNS are down
while the gateway/dns roles are down. TLS returns via fresh ACME
issuance (#792's universal recovery). The refound mints a new
`cluster_id` and new ULAs; everything re-derives, and redeploys
replace the orphaned containers blue/green and sweep the debris.
Old-DB stragglers surface as `doctor`'s foreign-`cluster_id`
finding, whose repair is the kit above.

The accepted bet, stated: adjacent-version Corrosion swaps are
expected to work in practice (the spike certified same-version
replacement), and refound is the floor if one ever doesn't. If
refound toil ever hurts on a grown cluster before the rolling
drill passes, a reseed command is the named upgrade path — decided
against, not designed.

## The mint list

Ordering is forced by one chain: rows need Corrosion, Corrosion binds
the ULA address, the ULA needs WG up — so identity and the interface
come from local files *before* any row exists (which is also why
Keeper's never-fold-an-empty-roster guard is safe on machine one).

```
 sudo ployz init:
  0  stage ployzd + pinned corrosion + docker-if-missing (release channel)
  1  mint cluster_id ULID → persist to /var/lib/ployz      ← resume anchor
  2  mint machine ULID + WG keypair → derive ULA /48 + machine /112
  3  mint cluster door keypair (TLS) → fingerprint kept for tokens
  4  allocate machine-one /24 from the chosen prefix       (first pick, no race)
  5  storage prep per resolved answer (zfs dataset root + docker
     drop-in | plain)
  6  write configs: corrosion (bind ULA, exact pin), daemon.json
     (insecure-registries mesh prefix), identity file
  7  write + enable per-role units; start keeper → wg0 up (own addr,
     no peers) → corrosion → api → gateway → dns
  8  write initial rows through the live api:
       cluster   {name, storage_default, hostname_mode, prefix,
                  provider, acme fields, v}
       machines  {machine one: transport + storage outcome/reason}
  9  api fold creates the `ployz` Docker network over the /24 (#801)
 10  enroll the driver: write its peers row (pubkey arrives over the
     driver's own channel: ssh | `--cloud-token`); Keeper converges
     the WG peer from the row — one writer of WG state
 11  if service URLs = ployz: reserve the managed wildcard, point it
     at this machine
 12  verify: trivial Corrosion query + barrier ready + /version
     answers → print summary + next step (`ployz token create`)
```

Every step is check-then-do; that is what makes resume free. Init is
bounded, local, and atomic per step — a write, not an operation. No
`default` namespace row is minted: namespaces are operator primitives,
and a first deploy without one refuses and names
`ployz namespace create`.

## Considered and rejected

- **Laptop-only SSH init** (uncloud's exact shape): orphans Cloud's
  paste-line bootstrap; the SSH engine becomes the only founding path.
- **On-host init + manual token paste for the laptop**: a second
  manual step the driver channel makes unnecessary.
- **Offline laptop init via a seed blob**: the blob can regenerate
  machine one's private key — a transported secret to save one paste.
- **Zero questions**: rejected once ZFS, the prefix, and the wildcard
  proved hard to change later; a default you cannot revisit deserves a
  question with a rationale.
- **Tailscale as a v1 question**: a front-door menu item whose answer
  is a refusal; the question ships when the provider does.
- **First token minted at init**: init stays "found the cluster";
  tokens are their own primitive.
- **Init owns reset-with-confirm** (uncloud's model): init becomes a
  destroyer and the refusal law bends; reset is its own named command.
- **A driver-side reset consent prompt**: init driving reset with
  extra steps; the named upgrade path if real operators complain
  about typing one command.
- **A `default` namespace row**: policy smuggled into the mint list.
- **Pure flag-else-gate storage (no `storage_default`)**: loses
  "this cluster is plain everywhere" as a one-time init answer; that
  operator would flag every join line forever.
