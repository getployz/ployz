# Cloud Bootstrap

Cloud bootstrap is an optional Bootstrap Delivery path for users who are
already SSHed into a target machine. The keeper bootstrap entrypoint owns
Cloud and custom-Cloud adoption; CLI-managed cluster creation stays owned by
`ployzctl machine init USER@HOST`.

The human command is:

```sh
curl -fsSL https://ployz.sh | sh && sudo ployz-keeper bootstrap
```

`ployz.sh` installs only the verified `ployz-keeper` binary. It does not carry
Cloud tokens, install `ployzctl`, choose a Cloud org, decide founder vs joiner,
or inspect machine bootstrap state. Keeper owns the prompt, optional Cloud
session, typed bootstrap envelope validation, local machine mutation, and
terminal callback when bootstrap uses Cloud.

The future automation command is:

```sh
curl -fsSL https://ployz.sh | sh && sudo ployz-keeper bootstrap --cloud-token pcbs_...
```

`--cloud-host <host-or-https-url>` may select staging or self-hosted Cloud for
Cloud bootstrap. It does not identify the target machine, org, or
cluster.

## Session And Token Modes

Interactive bootstrap creates a short-lived Cloud Bootstrap Session. Keeper
prints a browser URL with the non-secret user code prefilled, then polls Cloud
while the user chooses an organization in a browser on their workstation. V1
approval is one `Connect this machine` action; Cloud derives founder, joiner,
or wait behavior from that organization's Organization Cluster state. Future
founder options can be added to the browser approval flow without changing the
copied command.

Future token bootstrap may redeem a Cloud Bootstrap Token for cloud-init and
fleet automation. A token is a single-redemption bearer secret issued by a
time-limited Cloud Bootstrap Invite. The default invite duration is 1 hour;
multi-machine automation uses multiple tokens from the same invite.

Cloud bootstrap modes produce a Cloud Bootstrap Redemption. The redemption
returns a typed envelope with a callback URL, callback token, release
selection, and one intent: Founder Bootstrap, Joiner Bootstrap, or
wait-for-founder.

## Continue Without Cloud

`ployz-keeper bootstrap` does not create CLI-managed clusters. It still shows a
visible `Use local CLI setup` choice so Cloud remains optional and
discoverable. If the user chooses that path, keeper exits nonzero before
machine mutation, creates no Cloud Bootstrap Session, Cloud Bootstrap
Redemption, callback token, or Cloud Founder Claim, and tells the user to run:

```sh
ployzctl machine init USER@HOST
```

Handoff material from a machine-local keeper bootstrap to a workstation-local
`ployzctl` context is deferred until Ployz has an explicit import format,
secret-handling story, and cleanup behavior.

## Founder Bootstrap

Cloud-mediated Founder Bootstrap authorizes Cloud by NATS user public key. The
founder result callback is Cloud-safe: it returns the machine id, runtime NATS URL,
and trusted CA material, but not the local operator seed or Join seed.

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
be run with `sudo ployz-keeper uninstall` to remove Ployz substrate and
machine-local Ployz material from that machine, but it does not delete user
workloads, Docker images, Docker volumes, service containers, arbitrary
networks, or runtime data by default. Keeper removes its own binary as the
final step; failure to remove the binary is a leftover-binary warning after
substrate uninstall has otherwise completed. `sudo ployz-keeper uninstall`
requires interactive confirmation by default; `--yes` is the scripted bypass.
By default, uninstall refuses when local evidence says the machine is still an
accepted cluster member. `--force` overrides that local refusal and removes
local substrate anyway, but it does not perform Force Removed Machine or mutate
cluster truth. `--force` and `--yes` are independent: automation that wants
forced local cleanup must pass both.

If founder bootstrap succeeds locally but the Cloud callback fails, keeper
exits failed with local evidence and does not start a background retry worker.
Rerunning `sudo ployz-keeper bootstrap` on the same machine resumes the same
attempt and retries the terminal callback instead of creating a new Founder
Claim. Keeper persists the exact Cloud-safe terminal callback payload in
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
credential, runtime NATS URL, and trusted CA in the envelope. Keeper redeems the
join token against the cluster and reports terminal evidence. The Cloud callback
for success carries operation id, machine id, machine name, last event sequence,
and redeem result; it omits the join token and Join credential.

Joiner callback failure follows the same terminal-payload rule as founder
callback failure: keeper persists the exact Cloud-safe terminal payload before
posting it, exits failed with evidence if the post fails, starts no background
retry worker, and replays the persisted payload on rerun. If Cloud has already
accepted the callback, rerun exits success with current terminal Cloud status
evidence and performs no local mutation.

Duplicate reported hostnames are allowed as machine facts. Cloud derives a
unique current Machine Name before it submits core operations.

Joiner decisions depend on the Organization Cluster's Cloud Connection rather
than on a founder redemption's status.

## Local Direct Path

`ployzctl machine init USER@HOST` remains the workstation-driven local/direct
path. It is deterministic and noninteractive: `ployzctl` keeps the local
operator credential, bootstraps the first machine over SSH, activates it over
direct TLS NATS, and writes local Operator Context.

`ployzctl machine init --link-cloud` and `ployzctl cloud link` are deferred
until Cloud and local operators can be authorized as distinct direct NATS
operator clients.
