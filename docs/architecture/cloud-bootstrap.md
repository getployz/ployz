# Cloud Bootstrap

Cloud bootstrap is Bootstrap Delivery for users who are already SSHed into a
target machine.

The human command is:

```sh
curl -fsSL https://ployz.sh | sh && sudo ployz-keeper bootstrap
```

`ployz.sh` installs only the verified `ployz-keeper` binary. It does not carry
Cloud tokens, install `ployzctl`, choose a Cloud org, decide founder vs joiner,
or inspect machine bootstrap state. Keeper owns the prompt, Cloud session or
token redemption, typed bootstrap envelope validation, local machine mutation,
and terminal callback.

The automation command is:

```sh
curl -fsSL https://ployz.sh | sh && sudo ployz-keeper bootstrap --cloud-token pcbs_...
```

`--cloud-host <host-or-https-url>` may select staging or self-hosted Cloud. It
does not identify the target machine, org, or cluster.

## Session And Token Modes

Interactive bootstrap creates a short-lived Cloud Bootstrap Session. Keeper
prints a browser URL and code, then polls Cloud while the user chooses org,
cluster, and intent in a browser on their workstation.

Token bootstrap redeems a Cloud Bootstrap Token for cloud-init and fleet
automation. A token is a bearer secret for a time-limited Cloud Bootstrap
Invite. The default invite duration is 1 hour, and redemptions are not bounded
while the invite remains valid.

Both modes produce a Cloud Bootstrap Redemption. The redemption returns a typed
envelope with a callback URL, callback token, release selection, and one intent:
Founder Bootstrap, Joiner Bootstrap, or wait-for-founder.

## Founder Bootstrap

Cloud-mediated Founder Bootstrap authorizes Cloud by NATS user public key. The
founder result callback is Cloud-safe: it returns the node id, runtime NATS URL,
and trusted CA material, but not the local operator seed or Join seed.

Cloud proves founder usability with an outside-in direct TLS NATS probe. Local
public-IP checks can be diagnostics, but they are not proof that Cloud can reach
the control plane.

## Joiner Bootstrap

Joiner Bootstrap uses the existing machine-add and Machine Join Redemption
flow. Cloud may submit `machine.add`, then return the join token, Join user
credential, runtime NATS URL, and trusted CA in the envelope. Keeper redeems the
join token against the cluster and reports terminal evidence. The Cloud callback
for success carries operation id, node id, machine name, last event sequence,
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
