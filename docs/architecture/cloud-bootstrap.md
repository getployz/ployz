# Cloud Bootstrap

Cloud bootstrap is an optional Bootstrap Delivery path for users who are
already SSHed into a target machine. The Host Runner bootstrap entrypoint owns
Cloud and custom-Cloud adoption; CLI-managed cluster creation stays owned by
`ployz machine init USER@HOST`.

The human command is:

```sh
curl -fsSL https://ployz.sh | sh && sudo ployz host bootstrap cloud
```

`ployz.sh` installs only the verified `ployz host` binary. It does not carry
Cloud tokens, install `ployz`, choose a Cloud org, decide founder vs joiner,
or inspect machine bootstrap state. Host Runner owns the explicit Cloud
session, typed bootstrap envelope validation, local machine mutation, and
terminal callback when bootstrap uses Cloud.

The noninteractive automation command is:

```sh
curl -fsSL https://ployz.sh | sh && sudo ployz host bootstrap cloud --cloud-token pcbs_...
```

`bootstrap cloud --cloud-host <host-or-https-url>` may select staging or self-hosted Cloud for
Cloud bootstrap. It does not identify the target machine, org, or
cluster.

## Session And Token Modes

Interactive bootstrap creates a short-lived Cloud Bootstrap Session. Host Runner
prints a browser URL with the non-secret user code prefilled, then polls Cloud
while the user chooses an organization in a browser on their workstation. V1
approval is one `Connect this machine` action; Cloud derives founder, joiner,
or wait behavior from that organization's Organization Cluster state. Future
founder options can be added to the browser approval flow without changing the
copied command.
The v1 approval page is intentionally small: organization picker when needed,
minimal machine context, and the single `Connect this machine` action. It is an
approval screen, not a machine inspection surface, founder-options wizard, or
cluster configuration flow.
V1 does not need a standalone bootstrap-session list. Before approval, the
session exists only behind the approval URL. After approval, the Cloud
Bootstrap Redemption appears in the organization's machine-add/progress
surface as the machine being added; it must not be presented as an Accepted
Machine Identity until runtime acceptance exists.
If the session expires before browser approval, no Cloud Bootstrap Redemption
is created. Host Runner polling exits with an approval-expired message and tells the
user to rerun `sudo ployz host bootstrap`; rerunning creates a fresh Cloud
Bootstrap Session rather than reusing the expired one.
V1 approval has no explicit `Reject` action. Closing the page or doing nothing
leaves the session unapproved until it expires.

Token bootstrap redeems a Cloud Bootstrap Token for cloud-init and fleet
automation. Host Runner sends the token as HTTPS bearer authorization to
`POST /api/bootstrap/tokens/redeem` with its attempt, client, and machine facts.
Cloud returns the existing bootstrap decision shape. A token authorizes any
number of machine redemptions during its 24-hour lifetime; Cloud enforces expiry.

Cloud bootstrap modes produce a Cloud Bootstrap Redemption. The redemption
is created when Cloud approves a session or token for one machine use. For
interactive bootstrap, the browser `Connect this machine` approval creates the
durable redemption; Host Runner polling then receives the same redemption and
intent idempotently. The redemption returns a typed envelope with a callback
URL, callback token, and one intent: Founder Bootstrap, Joiner Bootstrap, or
wait-for-founder.

Cloud bootstrap does not make Cloud the release artifact authority. Host Runner and
the runtime own release-source resolution, artifact digest verification, and
the installed component set. Cloud must not carry release selections, artifact
URLs, or checksums.

## Continue Without Cloud

`ployz host bootstrap` does not create CLI-managed clusters and does not
start Cloud. It exits nonzero before machine mutation, creates no Cloud
Bootstrap Session, Cloud Bootstrap Redemption, callback token, or Cloud Founder
Claim, and tells the user to run:

```sh
ployz machine init USER@HOST
```

Handoff material from a machine-local Host Runner bootstrap to a workstation-local
`ployz` context is deferred until Ployz has an explicit import format,
secret-handling story, and cleanup behavior.

## Founder Bootstrap

Cloud-mediated Founder Bootstrap authorizes Cloud by NATS user public key. The
founder result callback is Cloud-safe: it returns the machine id, runtime NATS URL,
and trusted CA material, but not the local operator seed or Join seed.
Founder Bootstrap receives the runtime NATS URL and Cloud NATS user public key
from Cloud. Host Runner builds the first-machine install spec locally from its
release source and verifies every artifact locally before activation.
Fresh-machine approvals for the same Organization Cluster serialize through a
single sticky Cloud Founder Claim. The first approved redemption that wins the
claim receives Founder Bootstrap; other approved redemptions wait for the
founder to establish a Cloud Connection, be abandoned, or expire. Waiters are
not automatically promoted to founder; Abandon Founder Attempt rejects them.
Waiting Cloud Bootstrap Redemptions are polling-only from the target machine's
perspective. Host Runner prints that it is waiting for the first machine to finish,
honors Cloud retry hints, and performs no local mutation until Cloud has a
Cloud Connection and returns a Joiner Bootstrap envelope.
Waiting redemptions have a post-approval expiry independent of the pre-approval
Cloud Bootstrap Session expiry. The default waiting redemption TTL is 1 hour
from approval. Host Runner prints the remaining wait deadline while polling.
If a waiting redemption expires, it is terminal and cannot later receive a
Joiner Bootstrap envelope; Host Runner tells the user to rerun
`sudo ployz host bootstrap` for a fresh session and approval.
Cloud does not submit `machine.add`, mint join material, or issue the Joiner
Bootstrap envelope at waiter approval time. It does that only when the waiting
Host Runner polls after Cloud Connection exists, so runtime authority is created
close to actual join use.

Cloud proves founder usability with an outside-in direct TLS NATS probe. Local
public-IP checks can be diagnostics, but they are not proof that Cloud can reach
the control plane. If that probe fails after local founder success, the
founder Cloud Bootstrap Redemption or Cloud Founder Claim becomes
formed-but-unreachable; no Cloud Connection exists yet, and waiting redemptions
remain blocked.
Cloud stores the callback-reported endpoint, CA, and Cloud client material on
the founder redemption or Founder Claim until reachability succeeds. The Cloud
Connection row is created only after Cloud authenticates successfully.
If the user picked a machine that cannot be exposed to Cloud, they use Abandon
Founder Attempt. Cloud marks the formed-but-unreachable Founder Claim terminal,
rejects waiting redemptions, and allows a new Cloud Bootstrap Session to claim
Founder Bootstrap. The original machine is left as an unmanaged local cluster;
cleanup is explicit local work outside Cloud bootstrap. Substrate Uninstall can
be run with `sudo ployz host uninstall` to remove Ployz substrate and
machine-local Ployz material from that machine, but it does not delete user
workloads, Docker images, Docker volumes, service containers, arbitrary
networks, or runtime data by default. Host Runner removes its own binary as the
final step; failure to remove the binary is a leftover-binary warning after
substrate uninstall has otherwise completed. `sudo ployz host uninstall`
requires interactive confirmation by default; `--yes` is the scripted bypass.
By default, uninstall refuses when local evidence says the machine is still an
accepted cluster member. `--force` overrides that local refusal and removes
local substrate anyway, but it does not perform Force Removed Machine or mutate
cluster truth. `--force` and `--yes` are independent: automation that wants
forced local cleanup must pass both. `--yes` only skips waiting for input; it
does not suppress removal plans, accepted-machine evidence, or warnings.
That refusal is a hard stop before the normal confirmation prompt. Host Runner
prints the accepted-machine evidence it found, tells the user to remove the
machine from the cluster first and rerun uninstall, and shows `--force` as the
local-only escape hatch when cluster removal is impossible.
When `--force` is used with accepted-machine evidence present, Host Runner prints a
second warning before confirmation: local substrate removal does not remove the
machine from cluster truth, and cluster cleanup remains explicit operator work.
The warning shows `ployz machine remove --force <machine>` as the follow-up
cluster cleanup command when the user still has an operator context.
With `--force --yes`, Host Runner still prints the evidence and cluster-cleanup
warning before removing local substrate without prompting.
The refusal check uses Accepted Machine Evidence: accepted machine id state,
NATS machine credentials, role authority material, or assigned substrate state.
Failed or abandoned bootstrap attempt state, the Host Runner binary, and generic
install residue do not require `--force`.
If no Accepted Machine Evidence and no removable Ployz substrate or material
remain, uninstall reports that the machine is already clean and exits 0 without
prompting. This no-op path does not bypass accepted-machine refusal.
If uninstall removes some substrate or machine-local material but fails to
remove other required items, it exits nonzero and prints the remaining-items
list plus rerun guidance. The Host Runner binary remains the only warning-only
exception: if all other substrate and machine-local material are gone, a Host Runner
self-removal failure exits 0 with a leftover-binary warning.

If founder bootstrap succeeds locally but the Cloud callback fails, Host Runner
exits failed with local evidence and does not start a background retry worker.
Rerunning `sudo ployz host bootstrap` on the same machine resumes the same
attempt and retries the terminal callback instead of creating a new Founder
Claim. Host Runner persists the exact Cloud-safe terminal callback payload in
root-owned attempt state before posting it; reruns replay that payload and
refuse to recompute or mutate founder state for the terminal attempt.
If Cloud has already accepted the callback, rerun exits success with current
terminal Cloud status evidence and performs no local mutation. A Cloud
Connection exists only after Cloud can authenticate to the cluster as an
authorized NATS client.
The Cloud Connection is an Organization Cluster-level product relationship,
separate from the per-machine Cloud Bootstrap Redemption that created it.

## Joiner Bootstrap

Joiner Bootstrap uses the existing machine-add and Machine Join Redemption
flow. Cloud may submit `machine.add`, then return the join token, Join user
credential, runtime NATS URL, and trusted CA in the envelope. Host Runner redeems the
join token against the cluster and reports terminal evidence. The Cloud callback
for success carries operation id, machine id, machine name, last event sequence,
and redeem result; it omits the join token and Join credential.
Joiner Bootstrap does not receive artifact URLs, checksums, or a Cloud-chosen
install version. A joining machine follows the existing cluster's runtime
authority after redeeming join material; Host Runner asks the cluster/runtime for the
version and release source it should use.
After `machine.add` produces join material for one Cloud Bootstrap Redemption,
Cloud binds that redemption to the resulting Joiner Bootstrap envelope. If the
Host Runner response is lost, later polls return the same envelope rather than
creating another Machine Reservation or join token. If the Cloud-to-runtime
call fails before join material exists, Cloud retries that same step.

Joiner callback failure follows the same terminal-payload rule as founder
callback failure: Host Runner persists the exact Cloud-safe terminal payload before
posting it, exits failed with evidence if the post fails, starts no background
retry worker, and replays the persisted payload on rerun. If Cloud has already
accepted the callback, rerun exits success with current terminal Cloud status
evidence and performs no local mutation.

Duplicate reported hostnames are allowed as machine facts. Cloud derives a
unique current Machine Name before it submits core operations.

Joiner decisions depend on the Organization Cluster's Cloud Connection rather
than on a founder redemption's status.

## Local Direct Path

`ployz machine init USER@HOST` remains the workstation-driven local/direct
path. It is deterministic and noninteractive: `ployz` keeps the local
operator credential, bootstraps the first machine over SSH, activates it over
direct TLS NATS, and writes local Operator Context.

`ployz machine init --link-cloud` and `ployz cloud link` are deferred
until Cloud and local operators can be authorized as distinct direct NATS
operator clients.
