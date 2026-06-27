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
terminal callback when Cloud is connected.

The future automation command is:

```sh
curl -fsSL https://ployz.sh | sh && sudo ployz-keeper bootstrap --cloud-token pcbs_...
```

`--cloud-host <host-or-https-url>` may select staging or self-hosted Cloud for
Cloud-connected bootstrap. It does not identify the target machine, org, or
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

Cloud-connected modes produce a Cloud Bootstrap Redemption. The redemption
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
the control plane.

If founder bootstrap succeeds locally but the Cloud callback fails, keeper
exits failed with local evidence and does not start a background retry worker.
Rerunning `sudo ployz-keeper bootstrap` on the same machine resumes the same
attempt and retries the terminal callback instead of creating a new Founder
Claim.

## Joiner Bootstrap

Joiner Bootstrap uses the existing machine-add and Machine Join Redemption
flow. Cloud may submit `machine.add`, then return the join token, Join user
credential, runtime NATS URL, and trusted CA in the envelope. Keeper redeems the
join token against the cluster and reports terminal evidence. The Cloud callback
for success carries operation id, machine id, machine name, last event sequence,
and redeem result; it omits the join token and Join credential.

Duplicate reported hostnames are allowed as machine facts. Cloud derives a
unique current Machine Name before it submits core operations.

## Local Direct Path

`ployzctl machine init USER@HOST` remains the workstation-driven local/direct
path. It is deterministic and noninteractive: `ployzctl` keeps the local
operator credential, bootstraps the first machine over SSH, activates it over
direct TLS NATS, and writes local Operator Context.

`ployzctl machine init --link-cloud` and `ployzctl cloud link` are deferred
until Cloud and local operators can be authorized as distinct direct NATS
operator clients.
