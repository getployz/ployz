# Real-world hosts axis

Users enroll machines that a lab never makes: a desktop named `HP-WIN11`, distro Docker, a full disk, an FQDN hostname. Every soak and every pre-release check runs this axis against the release under test. It drives the user paths only, each through to a first deploy:

- the install line (`curl -fsSL https://ployz.sh | sudo sh -s -- VERSION --enroll`, token at the prompt);
- cloud-init user data with `PLOYZ_FLEET_TOKEN`;
- `ployz server add --command`, pasted on the host with `PLOYZ_ENROLL_TOKEN` exported;
- `ployz server add USER@HOST` over SSH as a non-root sudo user.

Use one or two Machines at a time and vary several conditions on each. Order a Machine's rows so the ones that fail before enroll (Docker refusals, clock, IPv6, hostname) come first and a clean join comes last, then deploy. Down everything after.

## Matrix

| Condition | Platform | How |
|---|---|---|
| Hostname with capitals, an FQDN, 64 characters | Incus or Boat | `hostnamectl set-hostname HP-WIN11` or `web-1.example.com`; `hostname $(printf 'a%.0s' {1..64})`. Ployz reads `/proc/sys/kernel/hostname`. Enroll with Names: Hostnames. |
| Underscore or unicode hostname | (none) | `hostnamectl` strips `_` and unicode; `hostname` refuses unicode on Ubuntu 26.04. Confirm, then mark it unreachable. |
| Newest Ubuntu, newest Debian, an older LTS | Incus | `images:ubuntu/26.04/cloud`, `images:debian/13/cloud`, `images:ubuntu/22.04/cloud`. Boat has only Ubuntu 24.04 and nested Debian 12. |
| Docker already installed, current | Boat | The bare base runs Docker with buildx and the containerd store. |
| Distro Docker, too old | Incus | `apt-get install docker.io`, take the refusal, then follow the documented `apt-get remove docker.io` and rerun (#3153). |
| ufw or nftables already on | Incus | cloud-init: `ufw default deny incoming; ufw allow 22; ufw --force enable` plus a custom `nft` table. Join a second Server through it and ping across `ployz-wg`. |
| IPv6-only | Boat | Lab host has no IPv6. Point resolved at an IPv6 resolver and `iptables -I OUTPUT -o <v4 nic> -m conntrack --ctstate NEW -j REJECT`; dropping the v4 route cuts your SSH (#3154). |
| Dual-stack | Boat | Every Boat sandbox. |
| Root under 30 GiB | Incus | `-d root,size=20GiB`. `server add` accepts it by design; the Fleet row shows `disk_too_small`. |
| Nearly full and full disk | Boat or Incus | `fallocate` to 2 GB free, then to 0; then `dd` to fill root's reserve too. Deploy a cached image and an uncached one. |
| `/var/lib/docker` on its own disk | Incus | Attach a block volume, `mkfs.ext4`, mount it before install. Images still land in `/var/lib/containerd` on root (#3155). |
| 1 GiB RAM | Incus | `limits.memory=1GiB`; join and run a Service. |
| Slow or flaky network | Boat | `tc qdisc add dev <nic> root netem delay 300ms 100ms loss 5%`, then `delay 800ms 400ms loss 20%`, on both NICs, during install and deploy. |
| Clock skew | Boat or Incus | `timedatectl set-ntp false; date -s '+3 min'`, then `+6 min`. Under 5 minutes passes; above it ends with `clock is …s off`. Restore with `set-ntp true`. |
| Non-root sudo user | Incus | cloud-init `users:` with `NOPASSWD` sudo and your key, then `ployz server add deploy@HOST`. Debian's cloud image has no sshd until you install `openssh-server`. |

## Lab

- **Cloud.** Use a local Cloud only, started as `VERIFY_REAL_SERVERS=1 VERIFY_IDLE_TIMEOUT=0 scripts/verify/up.sh`, which starts Inngest and the worker with no cluster. Without them, enroll progress never leaves **Starting**. The seed's `grace` and `linus` organizations start empty, which makes them first-Server orgs. A rerun of `up.sh` reseeds and can change the port.
- **Tokens.** A Fleet Token needs a signed-in person: click **Get install command** as Grace or Linus. For the CLI, `ployz login --json` prints a device URL; approve it in that person's browser session. `server add --command` works with the seed's `cliToken` in `ada`.
- **Incus.** Use a project of your own, its own bridge, and `iptables -I DOCKER-USER -i/-o <bridge> -j ACCEPT`, or the VMs have no internet. `incus init` reads stdin, so give it `</dev/null`. Vite binds 127.0.0.1, so forward the bridge address with `socat TCP-LISTEN:<port>,bind=<bridge ip>,fork TCP:127.0.0.1:<port>` and put that URL in the line.
- **Boat.** Use `verify-cluster up --bare`, `boat extend` at once, and `ssh -R <port>:localhost:<port>` with the run's `private/id_ed25519` for the Cloud. Set `PLOYZ_CLOUD_URL` to the forwarded port; the bare host has no other route to it.
- **Public IP.** An Incus Server detects this lab host's own public address. That is a production machine, so set `ployz server set <server> --public-ip <vm address>` right after it joins, before Cloud probes port 80.
- Change a waiting host's hostname, then let its next check-in report it. Restarting `ployz-checkin` mid-enroll breaks the attempt (#3157).

## Report

Send the supervisor a matrix of pass, fail and can't-test rows; give each can't-test row its reason. A first-Server or happy-path enroll failure is high.
