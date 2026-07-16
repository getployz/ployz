#!/usr/bin/env bash
# Bring up a single-machine Ployz core inside an init-less agent sandbox
# (the ephemeral VMs behind Claude Code on the web / Codex cloud).
#
# These sandboxes are Ubuntu, root-capable, and Docker-capable, but they run
# WITHOUT systemd (PID 1 is a custom process broker) and their only egress is
# a TLS-terminating HTTP proxy whose CA the stock `ployz` binary does not
# trust. This script works around both so `ployz host bootstrap core`
# completes and the NATS control plane comes up.
#
# It is a curiosity / demo aid, NOT a supported production install path.
# Everything it does is idempotent; re-running is safe.
#
# Usage:  sudo bash host-on-agent-vm.sh
set -euo pipefail

CHANNEL="${PLOYZ_CHANNEL:-alpha}"
STAGE="${PLOYZ_STAGE_DIR:-/opt/ployz-stage}"
CA_BUNDLE="${CCR_CA_BUNDLE:-/root/.ccr/ca-bundle.crt}"
SUPERNET="10.198.0.0/16"           # ployz_core DEFAULT_ENDPOINT_SUPERNET
NATS_PORT=4222
MACHINE_ID="${PLOYZ_MACHINE_ID:-core1}"

log() { printf '\n\033[1;36m== %s\033[0m\n' "$*"; }

require_root() { [ "$(id -u)" -eq 0 ] || { echo "run as root" >&2; exit 1; }; }

# 1. Trust the agent proxy CA so curl-based staging works. (The Rust binary
#    pins its own webpki roots and ignores this, which is why we stage the
#    substrate out-of-band below rather than letting bootstrap download it.)
install_proxy_ca() {
  log "Trusting agent proxy CA (for curl staging)"
  if [ -f "$CA_BUNDLE" ]; then
    cp "$CA_BUNDLE" /usr/local/share/ca-certificates/ccr-proxy.crt
    update-ca-certificates >/dev/null 2>&1 || true
  fi
}

# 2. A systemctl shim: these sandboxes have no init, but ployz's host-runner
#    supervises Docker, nats-server, and the ployzd roles via `systemctl`.
#    The shim launches the very unit files ployz writes as plain processes.
install_systemctl_shim() {
  log "Installing systemctl shim (no systemd in this sandbox)"
  local shim=/usr/local/bin/systemctl
  cat > "$shim" <<'SHIM'
#!/usr/bin/env python3
import os, re, signal, subprocess, sys, time
RUN, LOG, UNIT_DIR = "/run/ployz-shim", "/var/log/ployz-shim", "/etc/systemd/system"
os.makedirs(RUN, exist_ok=True); os.makedirs(LOG, exist_ok=True)
pidfile = lambda n: os.path.join(RUN, n + ".pid")
def read_pid(n):
    try:
        return int(open(pidfile(n)).read().strip())
    except Exception:
        return None
def alive(p):
    try:
        os.kill(p, 0); return True
    except Exception:
        return False
def stop(n):
    p = read_pid(n)
    if p and alive(p):
        try: os.killpg(os.getpgid(p), signal.SIGTERM)
        except Exception:
            try: os.kill(p, signal.SIGTERM)
            except Exception: pass
        for _ in range(50):
            if not alive(p): break
            time.sleep(0.1)
        if alive(p):
            try: os.killpg(os.getpgid(p), signal.SIGKILL)
            except Exception: pass
    try: os.remove(pidfile(n))
    except OSError: pass
def unit_path(n):
    if not n.endswith(".service"): n += ".service"
    return os.path.join(UNIT_DIR, n)
def strip(cmd): return re.sub(r'^[-@+!]+\s*', '', cmd)
def load_env_file(spec, into):
    opt = spec.startswith("-"); p = spec.lstrip("-")
    try:
        for line in open(p):
            line = line.strip()
            if line and not line.startswith("#") and "=" in line:
                k, v = line.split("=", 1); into[k] = v.strip().strip('"')
    except FileNotFoundError:
        if not opt: raise
def start_docker():
    if read_pid("docker") and alive(read_pid("docker")): return 0
    if subprocess.call(["docker","info"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL) == 0:
        return 0
    lf = open(os.path.join(LOG,"docker.log"),"ab")
    pr = subprocess.Popen(["dockerd"], stdout=lf, stderr=lf, stdin=subprocess.DEVNULL, start_new_session=True)
    open(pidfile("docker"),"w").write(str(pr.pid))
    for _ in range(60):
        if subprocess.call(["docker","info"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL) == 0:
            return 0
        time.sleep(0.5)
    return 1
def restart_docker():
    stop("docker"); subprocess.call(["pkill","-x","dockerd"]); time.sleep(1); return start_docker()
def start_unit(name):
    key = name[:-8] if name.endswith(".service") else name
    if key == "docker": return start_docker()
    env, files, pre, exec_start = {}, [], [], None
    for line in open(unit_path(name)):
        line = line.strip()
        if line.startswith("Environment="):
            kv = line[12:].strip().strip('"')
            if "=" in kv: k, v = kv.split("=",1); env[k] = v
        elif line.startswith("EnvironmentFile="): files.append(line[16:].strip())
        elif line.startswith("ExecStartPre="): pre.append(line[13:].strip())
        elif line.startswith("ExecStart="): exec_start = line[10:].strip()
    run_env = dict(os.environ)
    for f in files: load_env_file(f, run_env)
    run_env.update(env)
    for p in pre: subprocess.call(strip(p), shell=True, env=run_env)
    if not exec_start: return 1
    stop(key)
    lf = open(os.path.join(LOG, key + ".log"), "ab")
    pr = subprocess.Popen(strip(exec_start), shell=True, env=run_env, stdout=lf, stderr=lf,
                          stdin=subprocess.DEVNULL, start_new_session=True)
    open(pidfile(key),"w").write(str(pr.pid)); time.sleep(0.5)
    return 0 if alive(pr.pid) else 1
def main():
    argv = sys.argv[1:]
    now = "--now" in argv
    args = [a for a in argv if a not in ("--quiet","--no-block","--now")]
    if not args: return 0
    verb, units = args[0], args[1:]
    if verb in ("daemon-reload","disable","preset","mask","unmask"): return 0
    if verb == "enable":
        return sum(start_unit(u) for u in units) if now else 0
    if verb in ("start","restart","reload-or-restart","try-restart"):
        rc = 0
        for u in units:
            key = u[:-8] if u.endswith(".service") else u
            rc |= restart_docker() if key == "docker" else start_unit(u)
        return rc
    if verb in ("stop","kill"):
        for u in units:
            key = u[:-8] if u.endswith(".service") else u
            stop(key)
            if key == "docker": subprocess.call(["pkill","-x","dockerd"])
        return 0
    if verb == "is-active":
        rc = 0
        for u in units:
            key = u[:-8] if u.endswith(".service") else u
            ok = (subprocess.call(["docker","info"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)==0
                  if key=="docker" else alive(read_pid(key) or 0))
            if not ok: rc = 3
        return rc
    if verb == "is-enabled": print("enabled"); return 0
    return 0
sys.exit(main())
SHIM
  chmod +x "$shim"
  hash -r 2>/dev/null || true
}

# 3. Docker: start it and pre-seed daemon.json so bootstrap's Docker prep is a
#    no-op restart (containerd snapshotter + the internal-registry supernet as
#    an insecure registry).
prepare_docker() {
  log "Configuring and (re)starting dockerd"
  mkdir -p /etc/docker
  cat > /etc/docker/daemon.json <<EOF
{
  "features": { "containerd-snapshotter": true },
  "insecure-registries": [ "$SUPERNET" ]
}
EOF
  pkill -x dockerd 2>/dev/null || true
  rm -f /var/run/docker.sock /var/run/docker.pid 2>/dev/null || true
  sleep 1
  systemctl restart docker
  docker info --format '{{json .RegistryConfig.InsecureRegistryCIDRs}}'
}

# 4. Install the CLI from the public channel (this part already "just works").
install_cli() {
  log "Installing ployz CLI from '$CHANNEL' channel"
  curl -fsSL https://ployz.sh/ployz.sh -o /tmp/ployz-install.sh
  sh /tmp/ployz-install.sh --channel "$CHANNEL"
}

# 5. Stage the substrate offline. The stock binary can't fetch it (proxy CA),
#    and nats-server lives in a cross-repo GitHub release the sandbox token
#    can't reach (403) -- so we pull the exact version from Docker Hub. We then
#    hand bootstrap a file:// manifest whose artifact URLs are local paths;
#    the host-runner copies local-path sources directly (no TLS, no proxy).
stage_substrate() {
  log "Staging substrate into $STAGE (offline manifest)"
  mkdir -p "$STAGE"; cd "$STAGE"
  local rel; rel="$(curl -fsSL "https://ployz.sh/channels/${CHANNEL}.env")"
  local tag; tag="$(printf '%s\n' "$rel" | awk -F= '/PLOYZ_RELEASE_TAG=/{print $2}')"
  local ver; ver="$(printf '%s\n' "$rel" | awk -F= '/PLOYZ_VERSION=/{print $2}')"
  local base="https://github.com/getployz/ployz/releases/download/${tag}"
  local man; man="$(curl -fsSL "${base}/ployz-release-linux-amd64.env")"
  mval() { printf '%s\n' "$man" | awk -F= -v k="$1" '$1==k{print substr($0,length(k)+2)}'; }

  curl -fsSL "$(mval PLOYZD_URL)"        -o ployzd
  curl -fsSL "$(mval PLOYZ_EBPF_CTL_URL)" -o ployz-ebpf-ctl
  curl -fsSL "$(mval PLOYZ_EBPF_TC_URL)"  -o ployz-ebpf-tc

  local nver; nver="$(mval PLOYZ_NATS_SERVER_VERSION)"
  local cid; cid="$(docker create "nats:${nver}")"
  docker cp "$cid:/usr/local/bin/nats-server" ./nats-server 2>/dev/null \
    || docker cp "$cid:/nats-server" ./nats-server
  docker rm "$cid" >/dev/null
  chmod +x nats-server
  tar -czf nats-server.tar.gz nats-server
  local nsha; nsha="$(sha256sum nats-server.tar.gz | awk '{print $1}')"

  cat > release.env <<EOF
PLOYZ_VERSION=${ver}
PLOYZ_RELEASE_TAG=${tag}
PLOYZ_RELEASE_PLATFORM=linux-amd64
PLOYZ_URL=/usr/local/bin/ployz
PLOYZ_SHA256=$(mval PLOYZ_SHA256)
PLOYZD_URL=${STAGE}/ployzd
PLOYZD_SHA256=$(mval PLOYZD_SHA256)
PLOYZ_EBPF_CTL_URL=${STAGE}/ployz-ebpf-ctl
PLOYZ_EBPF_CTL_SHA256=$(mval PLOYZ_EBPF_CTL_SHA256)
PLOYZ_EBPF_TC_URL=${STAGE}/ployz-ebpf-tc
PLOYZ_EBPF_TC_SHA256=$(mval PLOYZ_EBPF_TC_SHA256)
PLOYZ_NATS_SERVER_VERSION=${nver}
PLOYZ_NATS_SERVER_URL=${STAGE}/nats-server.tar.gz
PLOYZ_NATS_SERVER_SHA256=${nsha}
EOF
  echo "staged manifest: ${STAGE}/release.env"
}

# 6. Bootstrap the core. host-ports-assured-externally skips firewalld probing
#    (no systemd), gateway=skip keeps it to the control plane.
bootstrap_core() {
  log "Bootstrapping core (offline manifest, shimmed init)"
  local ip; ip="$(hostname -I 2>/dev/null | awk '{print $1}')"; ip="${ip:-127.0.0.1}"
  PLOYZ_RELEASE_MANIFEST_URL="file://${STAGE}/release.env" \
  PLOYZ_MACHINE_ID="$MACHINE_ID" \
  PLOYZ_MACHINE_JOIN_NATS_URL="tls://127.0.0.1:${NATS_PORT}" \
  PLOYZ_MACHINE_JOIN_CLUSTER_NAME="ployz" \
  PLOYZ_MACHINE_PUBLIC_IP="$ip" \
  PLOYZ_GATEWAY="skip" \
  PLOYZ_HOST_PORTS_ASSURED_EXTERNALLY=1 \
    ployz host bootstrap core
}

require_root
install_proxy_ca
install_systemctl_shim
install_cli
prepare_docker
stage_substrate
bootstrap_core

log "Done. Substrate processes:"
for p in /run/ployz-shim/*.pid; do
  [ -e "$p" ] || continue
  n="$(basename "$p" .pid)"; pid="$(cat "$p")"
  printf '  %-24s pid=%s %s\n' "$n" "$pid" "$(kill -0 "$pid" 2>/dev/null && echo up || echo down)"
done
echo
echo "The bootstrap output above includes ca_pem / operator_seed / join_seed."
echo "Point the CLI at the core with:"
echo "  PLOYZ_NATS_URL=tls://127.0.0.1:${NATS_PORT} \\"
echo "  PLOYZ_NATS_CA_FILE=<ca.pem> PLOYZ_NATS_NKEY_SEED_FILE=<operator.seed> \\"
echo "  PLOYZ_JOIN_NKEY_SEED_FILE=<join.seed> ployz ops list"
