# Ployz design

Ployz deploys containers across a Cluster of user-owned Docker Machines joined
by a flat WireGuard mesh. There is no control plane. Every Machine is an equal
Entry Machine, state replicates as CRDTs, and the CLI and the dashboard are
equal surfaces over one Config Store that Cloud hosts per Organization.
Capitalized terms mean what [CONTEXT.md](CONTEXT.md) says.

## How to use this document

Read the bets before designing. In the PR body, name each bet your change
touches and the choice it changed. If the change crosses a red flag, argue it
there. A red flag is a demand for an argument, not an automatic no. Edit a bet
only when the bet itself changes.

This file states positions. Mechanism lives in the doc comments of the code
that owns it, and user-visible behavior in [docs/user/](../docs/user/).

## Stable promise

**Bet.** From 0.3.0, a daemon keeps working and upgrades across every later
0.x release without re-enrolling. Only a new major line may break that, and it
says so. The promise covers what a daemon carries or speaks: replicated rows
and bodies, the local Machine record, Machine RPC within `PROTOCOL_MAJOR`,
enrollment, the release source, and the Internal DNS spec and probe. The CLI
is a client courtesy with ordinary deprecation. The Observe Store is
disposable and outside the promise.

**Before 0.3.0.** 0.3.0 is the first release the promise covers. A Machine on
a 0.2 release or a 0.3.0 beta is outside it, yet still upgrades to 0.3.0 in
place without re-enrolling. On 0.2 or 0.3.0-beta.6 and earlier, the first
Machine of a Cluster takes that upgrade over SSH, because its relay protocol
predates 0.3.0's; the rest upgrade through it.

**The 0.4 authority exception.** From 0.4, a Machine refuses a route that
changes it when the request carries no customer authority, and names the
route's capability. This is the only semantic 0.x has withdrawn. It is not a
precedent. The next withdrawal waits for a major.

**The 0.4 observe exception.** 0.4 deletes the observe history a Machine held
and needs every Service redeployed to log the new way. It withdraws no route,
and it is not a precedent: the Observe Store stays outside the promise.

**Rules.** Frozen formats only gain optional fields with defaults. Nothing is
renamed or repurposed, and readers ignore unknown fields. Replicated bodies
never refuse unknown fields; only secret-bearing sections and
verification-selecting modes outside them may fail closed. Authored config and
unrelated trust boundaries stay strict. No version gates,
no migrations: a change that cannot be additive waits for the next major.
Machine RPC evolves by capability, never by version compare:

1. A new feature is a new catalog row with a new capability name. A name never
   changes meaning.
2. New response, stream and watch fields are optional. Absent claims nothing.
3. A new request field must be harmless to a daemon that ignores it, or it is
   a new route.
4. Changed semantics get a new `.vN` route. The old row serves until a major.
5. A new stream variant is opt-in through a request field.
6. A removed verb is shimmed, or the daemon stops advertising it and refuses.
7. Every refusal of a catalogued route names `details.capability`.
8. The one exception to rule 4: when an old route's meaning can destroy user
   data, it narrows in place to keep the data, and an older caller gets the
   safe outcome instead of the destructive one. Each such narrowing is listed
   here with its cost:
   - `ensure.v1` and `ensure-pinned.v1` keep a removed Container's anonymous
     volumes unless the removal names the copy that replaced it (#4562). An
     older runner's in-place replace leaves zero to many volumes behind.
   - A Volume Forget refuses a slot that may hold writes no other Machine has
     unless it says `discard` (#4571). A self-hosted Cloud on 0.4.0-beta.2 or
     earlier can't remove a Move's old copy until it upgrades.

A breaking line protects older Clusters from its own side: daemons follow
their own line's channel, and the new line refuses to mix with the old.

**Red flags.** A required new field, a migration, a replicated reader that
rejects unknown fields, behavior that needs every peer upgraded.

**Test.** Can a daemon one release older read every row this change writes,
and refuse every new route by capability name?

## 1. Observer-relative truth

**Bet.** Every view of a Cluster is one Machine's observation at one time.
Nothing declares authoritative Cluster state.

**Why.** "The" Cluster state needs consensus we refuse to pay for. Weak
semantics stated honestly beat strong semantics enforced badly.

**Red flags.** A fencing token, lease, leader election or quorum read, and any
API claiming a complete Cluster view. The one allowed shape: a request names
the copy it expects on one Machine, and carries the run number that Machine's
own floor checks. Neither number goes in Corrosion.

**Test.** Does the answer say whose observation it is and when?

## 2. AP over C inside the Cluster

**Bet.** Availability and partition tolerance beat consistency. Each partition
stays operable on its own, and state converges when it heals. Cloud-side
Organization state may be consistency-first; that stops at the Cloud boundary.

**Why.** A mesh of user-owned Machines partitions routinely. An operation that
waits for quorum turns every partition into an outage.

**Red flags.** A Cluster operation blocked on quorum, a partition treated as
an error, Cloud-style consistency imported into mesh behavior.

**Test.** Does it still work on the smaller side of a split?

## 3. Bounded imperative commands

**Bet.** A command calculates against a snapshot, executes, reports, and
stops. Nothing places, moves or reconciles Services in the background. A
Volume Run is the one durable workflow, a fixed list of steps a user asked for
that ends and never restarts itself. A removal mark is the one persisted
intent, finished by later admissions, never by a timer.

**Why.** Imperative errors reach the caller that can act on them. Reconcilers
hide failures behind "it will fix itself later."

**Red flags.** Controllers, persisted desired state, Cluster-wide reconcilers,
work that continues after its command returns. Machine-local loops that only
observe or extend Docker's restart policy to its own serving infrastructure,
and a `SetVolumeCopy` task that ends when ZFS or Docker answers, are the
accepted exceptions; each states its bound where it is coded.

**Test.** When the command returns, has everything it started either finished
or been reported?

## 4. Partial results are outcomes

**Bet.** A fan-out returns successes with per-target failures and omissions.
A Deploy may finish only a prefix of its plan. There is no atomicity and no
general rollback, only narrow explicit compensation within one release line.

**Why.** Faking atomicity across Machines means lying in the result or
building the coordination bet 1 forbids. The true prefix lets a person or a
retry act on facts.

**Red flags.** All-or-nothing semantics, automatic rollback, per-target detail
collapsed into one boolean.

**Test.** After a failure halfway, can the caller see exactly which half
happened?

## 5. Two-tier identity

**Bet.** Entities (Machines, Containers) get an opaque ID their creator mints
alone, and duplicate names coexist forever. Declared, replicated facts are
name-keyed: concurrent writes converge last-writer-wins, and the winner's ID
is the handle history attaches to.

**Why.** Nobody can enforce unique names without a global authority, so
ambiguity and merge loss are embraced, not half-prevented.

**Red flags.** An ID registry, fencing on merge loss, assuming a name
resolves to one thing, a name used as identity or an ID as a grouping key,
absence from one view taken as non-existence.

**Test.** What happens when two Machines create the same name at once?

## 6. Machine-local authority

**Bet.** A Machine rules only what is on itself: its Volumes, subnets,
addresses and Containers, decided from its own Pool, Docker and records. Work
across Machines is a sequence one operator drives. Cloud or the CLI is that
operator, and the operator also allocates. Each Machine admits or refuses each
step against its own facts. Two operators at once may collide, and Ployz does
not detect, prevent or repair that. A late step from a dead run can at worst
leave a Volume with no Writer, never two. A copy restored elsewhere while its
old Machine was away may take writes until Cloud demotes the older epoch; that
window is accepted, not prevented. A Machine starts no second running Service
Container on a Volume unless the one it starts carries the Volume's Shared
Writes in its signed spec: the user's consent to several writers, frozen at its
Deploy, so the Machine decides from that spec alone. A stale Create of a
sharing Container may so run beside a Writer after sharing was turned off;
that is accepted too. Docker's own restart of a stopped Container, by
restart policy or when dockerd restarts, bypasses Ployz. A dockerd restart
during a stop-first handoff can briefly run two on one Volume (#4467); that is
accepted too.

**Why.** In an eventually consistent Cluster, a Machine knows only itself for
certain. A single writer inside the Cluster needs the consensus bet 2 refuses.
One outside it is free: Cloud has Postgres, the CLI has a disk.

**Red flags.**

- A Machine acting on another Machine's records. A replicated observation may
  narrow what it does, never authorize an effect.
- A lease that must reach Machines the run does not touch.
- A barrier across Machines.
- A clock-based safety wait. A driver bounding its own patience is fine, and
  so is a drain it asks a Machine to wait before stopping its own Container.
- A resource identity meaningful without its Machine.
- Machinery to detect, prevent or repair operator collisions, or a single
  writer inside the Cluster.

**Test.** Could this Machine make the same decision if every other Machine
were unreachable?

## 7. Cloud drives, never owns

**Bet.** Cloud hosts the Config Store and drives Deploys and Volume Runs, but
holds no runtime truth. Its rows are an operator's bookkeeping. It reaches
Machines over the same Machine RPC seam as SSH. Cloud may store the observe
data Machines send it, but holds no runtime truth a Cluster needs: Clusters
deploy, run and tail live logs without Cloud. A verb lives in `ployz` if a cloudless Cluster needs it;
otherwise Cloud publishes it as data.

**Why.** The Cluster must stay operable without Cloud. Without an operator, a
Volume stays put and everything else still works. Observe data is history, not
truth: losing Cloud's copy loses nothing a Machine decides with, while history
kept only on a Machine is gone when its Server is.

**Red flags.** Cloud-held runtime truth, a connection catalog treated as
membership, transport failure used to forget or reset a Cluster, a Cluster
operation whose correctness needs Cloud, a Machine shipping a Sealed value it
knows unmasked, Cloud trusting a sender's labels to say which Organization a record belongs to.

**Test.** If Cloud vanished mid-operation, is every Machine still correct?

## 8. Evidence over claims

**Bet.** Report exactly what was observed, completed, failed and never
attempted. A diagnosis names the earliest proven failure stage, or Unknown. A
503 alone is not capacity evidence.

**Why.** When every view is partial, a confident guess is a lie with good
posture.

**Red flags.** Certainty the observer lacks, error messages that guess at
causes, success that hides unattempted work, an unread value shown as zero.

**Test.** Is every claim in the output backed by something this observer saw?

## 9. Lean on proven primitives

**Bet.** Docker for containers, WireGuard for the mesh, SQLite-backed CRDTs
for state, Caddy as the only Ingress Proxy, systemd for lifecycle, ZFS for
Volumes. Ployz writes only the thin coordination between them.

**Why.** Every primitive we own is one we patch, secure and debug forever. The
budget belongs to the coordination nobody else will build.

**Red flags.** Hand-rolled consensus, custom overlay networking, bespoke TLS,
tuning a primitive to hide a coordination gap, reimplementing what a proven
component already does.

**Test.** Does a mainstream component already do this?

## 10. Client-first, daemon when forced

**Bet.** Behavior lands in the client. The daemon owns only what must continue
without a client, enforce a Machine-local safety boundary, or manage a
Machine-local resource. Serving infrastructure (Corrosion, Internal DNS, the
Observe Store) is daemon-owned but outlives every daemon stop, crash, restart
and upgrade. The Ingress Proxy, its holder and Caddy, is the only Service
Container the daemon starts unasked.

**Why.** A client update ships instantly. A daemon change must reach every
Machine in every Cluster.

**Red flags.** Daemon policy without one of the three reasons, daemon logic a
client could compute from observations it already has, a daemon stop path
that stops serving, a second writer of a serving unit's spec, DNS answering
before its first load.

**Test.** Which of the three reasons forces this into the daemon?

## Open questions

Current behavior kept on purpose until a product decision changes it. Decide
here before changing the code.

- **Corrosion bootstrap.** A joiner bootstraps from every peer visible at
  join, a founder from none.
- **Corrosion configuration.** The daemon writes `config.toml`; whether Ployz
  owns that format is undecided.
- **Internal DNS record types.** A only; everything else gets an empty
  NOERROR.
- **Internal DNS caching.** TTL zero, no cache, until stale answers are judged
  acceptable.
- **Membership in ingress.** The Ingress Proxy ignores Membership
  Observations; Internal DNS filters on them. Aligning them changes every
  replicated projection.

## Boundaries

Ployz does not provide an authoritative Cluster view, a central scheduler or
quorum, Cluster-wide atomic operations or general rollback, automatic
correction of every difference between observations, a single writer inside
the Cluster, an abstraction over container runtimes, or continuously
replicated storage. One point-in-time Mirror per Volume, refreshed on request,
is in scope. A feature that needs a boundary is fighting the design, not
filling a gap. Add a stronger guarantee only when the product needs it and the
system can prove it.
