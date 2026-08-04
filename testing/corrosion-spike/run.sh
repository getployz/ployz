#!/usr/bin/env bash
set -Eeuo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/../.." && pwd)"
RUNS_ROOT="${CORROSION_SPIKE_RUNS_ROOT:-$REPO_ROOT/.scratch/corrosion-spike}"
readonly RELEASE_MANIFEST="$REPO_ROOT/corrosion-release.json"
VULTR_API="https://api.vultr.com/v2"
REGION="${CORROSION_SPIKE_REGION:-ewr}"
PLAN="${CORROSION_SPIKE_PLAN:-vc2-1c-1gb}"
OS_ID="${CORROSION_SPIKE_OS_ID:-2284}"
CORROSION_SPIKE_PLATFORM="${CORROSION_SPIKE_PLATFORM:-linux-amd64}"
VULTR_OS_ARCH=""
RELEASE_TAG=""
RELEASE_ARCHIVE_NAME=""
RELEASE_URL=""
RELEASE_SHA256=""
RELEASE_EMBEDDED_VERSION=""
SSH_OPTIONS=()
TUNNEL_PIDS=()
RUN_DIR=""
STATE_FILE=""
CLEANUP_ARMED=false

usage() {
    printf '%s\n' \
        "Usage:" \
        "  VULTR_API_KEY=... $0                 # fresh certification; auto-cleanup" \
        "  VULTR_API_KEY=... $0 resume <run-directory> <phase>" \
        "  VULTR_API_KEY=... $0 cleanup <run-directory>"
}

require_command() {
    local command_name="$1"
    command -v "$command_name" >/dev/null 2>&1 || {
        printf 'missing required command: %s\n' "$command_name" >&2
        return 1
    }
}

load_release_manifest() {
    local pin_output
    local -a corrosion_pin
    case "$CORROSION_SPIKE_PLATFORM" in
        linux-amd64) VULTR_OS_ARCH="x64" ;;
        linux-arm64) VULTR_OS_ARCH="arm64" ;;
        *)
            printf 'unsupported Corrosion spike platform: %s (supported: linux-amd64, linux-arm64)\n' \
                "$CORROSION_SPIKE_PLATFORM" >&2
            return 1
            ;;
    esac

    if ! pin_output="$(python3 \
        "$REPO_ROOT/scripts/read-corrosion-release.py" \
        "$RELEASE_MANIFEST" \
        "$CORROSION_SPIKE_PLATFORM")"; then
        return 1
    fi
    mapfile -t corrosion_pin <<<"$pin_output"
    if [[ "${#corrosion_pin[@]}" -ne 5 ]]; then
        printf 'corrosion release manifest did not yield exactly five pinned fields\n' >&2
        return 1
    fi
    RELEASE_TAG="${corrosion_pin[0]}"
    RELEASE_ARCHIVE_NAME="${corrosion_pin[1]}"
    RELEASE_URL="${corrosion_pin[2]}"
    RELEASE_SHA256="${corrosion_pin[3]}"
    RELEASE_EMBEDDED_VERSION="${corrosion_pin[4]}"
    readonly RELEASE_TAG RELEASE_ARCHIVE_NAME RELEASE_URL RELEASE_SHA256
    readonly RELEASE_EMBEDDED_VERSION
}

require_api_key() {
    if [[ -z "${VULTR_API_KEY:-}" ]]; then
        printf 'VULTR_API_KEY must be set\n' >&2
        return 1
    fi
}

api() {
    local method="$1"
    local path="$2"
    local body="${3:-}"
    local args=(
        --fail-with-body
        --silent
        --show-error
        --request "$method"
        --header "Authorization: Bearer $VULTR_API_KEY"
    )

    if [[ -n "$body" ]]; then
        args+=(--header "Content-Type: application/json" --data "$body")
    fi

    curl "${args[@]}" "$VULTR_API$path"
}

state_replace() {
    local filter="$1"
    shift
    local next
    next="$(mktemp "$RUN_DIR/state.XXXXXX")"
    jq "$@" "$filter" "$STATE_FILE" >"$next"
    mv "$next" "$STATE_FILE"
}

instance_ip() {
    local node="$1"
    jq -er --arg node "$node" '.instances[] | select(.node == $node) | .main_ip' "$STATE_FILE"
}

ssh_node() {
    local node="$1"
    shift
    # Arguments are assembled locally and passed to the remote shell by ssh.
    # shellcheck disable=SC2029
    ssh "${SSH_OPTIONS[@]}" "root@$(instance_ip "$node")" "$@"
}

scp_to_node() {
    local source="$1"
    local node="$2"
    local destination="$3"
    scp "${SSH_OPTIONS[@]}" "$source" "root@$(instance_ip "$node"):$destination"
}

scp_from_node() {
    local node="$1"
    local source="$2"
    local destination="$3"
    scp "${SSH_OPTIONS[@]}" "root@$(instance_ip "$node"):$source" "$destination"
}

remote_command() {
    local node="$1"
    shift
    local command_string=""
    local argument
    for argument in "$@"; do
        printf -v command_string '%s%q ' "$command_string" "$argument"
    done
    ssh_node "$node" "$command_string"
}

remote_shell() {
    local node="$1"
    local script="$2"
    remote_command "$node" bash -lc "$script"
}

local_driver() {
    local node="$1"
    shift
    python3 "$SCRIPT_DIR/driver.py" "$@" \
        --api "127.0.0.1:$((18080 + node))" \
        --token-file "$RUN_DIR/token"
}

remote_driver() {
    local node="$1"
    shift
    remote_command "$node" \
        python3 /root/ployz-spike/driver.py "$@" \
        --api 127.0.0.1:8080 \
        --token-file /root/ployz-spike/token
}

download_release() {
    local cache_dir="$RUNS_ROOT/cache/$RELEASE_TAG"
    local archive="$cache_dir/$RELEASE_ARCHIVE_NAME"
    mkdir -p "$cache_dir"

    if [[ ! -f "$archive" ]] || ! printf '%s  %s\n' "$RELEASE_SHA256" "$archive" | sha256sum --check --status; then
        local partial="$archive.partial"
        rm -f "$partial"
        curl --fail --location --retry 3 --output "$partial" "$RELEASE_URL"
        printf '%s  %s\n' "$RELEASE_SHA256" "$partial" | sha256sum --check
        mv "$partial" "$archive"
    fi

    printf '%s\n' "$archive"
}

preflight() {
    require_api_key
    local command_name
    for command_name in curl jq openssl ssh scp ssh-keygen sha256sum sqlite3 tar python3; do
        require_command "$command_name"
    done
    load_release_manifest
    jq '.' "$RELEASE_MANIFEST" >"$RUN_DIR/evidence/corrosion-release.json"

    local plan_json
    plan_json="$(api GET "/plans?type=vc2&per_page=500" | jq -e \
        --arg plan "$PLAN" --arg region "$REGION" \
        '.plans[] | select(.id == $plan and (.locations | index($region)))')"
    printf '%s\n' "$plan_json" >"$RUN_DIR/evidence/vultr-plan.json"

    local os_json
    os_json="$(api GET "/os?per_page=500" | jq -e --argjson os_id "$OS_ID" \
        --arg os_arch "$VULTR_OS_ARCH" \
        '.os[] | select(.id == $os_id and .arch == $os_arch)')"
    printf '%s\n' "$os_json" >"$RUN_DIR/evidence/vultr-os.json"
}

create_ssh_key() {
    ssh-keygen -q -t ed25519 -N "" -C "$(jq -r .run_id "$STATE_FILE")" -f "$RUN_DIR/ssh_key"
    local public_key
    public_key="$(<"$RUN_DIR/ssh_key.pub")"
    local response
    response="$(api POST "/ssh-keys" "$(jq -cn \
        --arg name "$(jq -r .run_tag "$STATE_FILE")" \
        --arg ssh_key "$public_key" \
        '{name: $name, ssh_key: $ssh_key}')")"
    local key_id
    key_id="$(jq -er '.ssh_key.id' <<<"$response")"
    # jq variables are intentionally expanded by jq, not the shell.
    # shellcheck disable=SC2016
    state_replace '.ssh_key_id = $key_id' --arg key_id "$key_id"
}

create_instances() {
    local run_tag
    run_tag="$(jq -r .run_tag "$STATE_FILE")"
    local ssh_key_id
    ssh_key_id="$(jq -r .ssh_key_id "$STATE_FILE")"
    local node
    for node in 1 2 3; do
        local label="${run_tag}-node${node}"
        local hostname="${label,,}"
        local request
        request="$(jq -cn \
            --arg region "$REGION" \
            --arg plan "$PLAN" \
            --argjson os_id "$OS_ID" \
            --arg label "$label" \
            --arg hostname "$hostname" \
            --arg tag "$run_tag" \
            --arg ssh_key_id "$ssh_key_id" \
            '{
                region: $region,
                plan: $plan,
                os_id: $os_id,
                label: $label,
                hostname: $hostname,
                tags: [$tag],
                sshkey_id: [$ssh_key_id],
                backups: "disabled",
                activation_email: false,
                enable_ipv6: true
            }')"

        local response
        if ! response="$(api POST "/instances" "$request")"; then
            printf 'Vultr rejected instance request for %s: %s\n' \
                "$label" "$response" >&2
            return 1
        fi
        local instance_id
        instance_id="$(jq -er '.instance.id' <<<"$response")"
        # jq variables are intentionally expanded by jq, not the shell.
        # shellcheck disable=SC2016
        state_replace '.instances += [{
            node: $node,
            id: $instance_id,
            label: $label,
            main_ip: null
        }]' \
            --arg node "$node" \
            --arg instance_id "$instance_id" \
            --arg label "$label"
        printf 'created %s (%s)\n' "$label" "$instance_id"
    done
}

wait_for_instances() {
    local attempt
    for attempt in $(seq 1 180); do
        local ready=0
        local node
        for node in 1 2 3; do
            local instance_id
            instance_id="$(jq -r --arg node "$node" '.instances[] | select(.node == $node) | .id' "$STATE_FILE")"
            local response
            response="$(api GET "/instances/$instance_id")"
            local status
            status="$(jq -r '.instance.status' <<<"$response")"
            local power_status
            power_status="$(jq -r '.instance.power_status' <<<"$response")"
            local server_status
            server_status="$(jq -r '.instance.server_status' <<<"$response")"
            local main_ip
            main_ip="$(jq -r '.instance.main_ip' <<<"$response")"
            if [[ "$status" == "active" && "$power_status" == "running" && "$server_status" == "ok" && "$main_ip" != "0.0.0.0" ]]; then
                # jq variables are intentionally expanded by jq, not the shell.
                # shellcheck disable=SC2016
                state_replace '(.instances[] | select(.node == $node) | .main_ip) = $main_ip' \
                    --arg node "$node" \
                    --arg main_ip "$main_ip"
                ready=$((ready + 1))
            fi
        done

        if [[ "$ready" -eq 3 ]]; then
            return
        fi
        sleep 5
    done

    printf 'instances did not become ready in time\n' >&2
    return 1
}

wait_for_ssh() {
    SSH_OPTIONS=(
        -i "$RUN_DIR/ssh_key"
        -o "UserKnownHostsFile=$RUN_DIR/known_hosts"
        -o StrictHostKeyChecking=accept-new
        -o ConnectTimeout=10
        -o ServerAliveInterval=10
        -o ServerAliveCountMax=3
    )

    local node
    for node in 1 2 3; do
        local attempt
        for attempt in $(seq 1 60); do
            if ssh_node "$node" true 2>/dev/null; then
                break
            fi
            if [[ "$attempt" -eq 60 ]]; then
                printf 'SSH did not become ready on node %s\n' "$node" >&2
                return 1
            fi
            sleep 5
        done
    done
}

validate_instance_for_cleanup() {
    local instance_id="$1"
    local run_tag="$2"
    local response_file
    response_file="$(mktemp "$RUN_DIR/instance.XXXXXX")"
    local status
    status="$(curl --silent --show-error --output "$response_file" --write-out '%{http_code}' \
        --header "Authorization: Bearer $VULTR_API_KEY" \
        "$VULTR_API/instances/$instance_id")"
    if [[ "$status" == "404" ]]; then
        rm -f "$response_file"
        return 2
    fi
    if [[ "$status" != "200" ]]; then
        printf 'cannot validate instance %s for cleanup (HTTP %s)\n' "$instance_id" "$status" >&2
        rm -f "$response_file"
        return 1
    fi
    if ! jq -e --arg tag "$run_tag" \
        '.instance.tags | index($tag) != null' "$response_file" >/dev/null; then
        printf 'refusing to delete untagged instance %s\n' "$instance_id" >&2
        rm -f "$response_file"
        return 1
    fi
    rm -f "$response_file"
}

cleanup_run() {
    require_api_key
    local cleanup_dir="$1"
    local cleanup_state="$cleanup_dir/state.json"
    if [[ ! -f "$cleanup_state" ]]; then
        printf 'refusing cleanup without state file: %s\n' "$cleanup_state" >&2
        return 1
    fi

    RUN_DIR="$(cd -- "$cleanup_dir" && pwd)"
    STATE_FILE="$RUN_DIR/state.json"
    local run_tag
    run_tag="$(jq -er .run_tag "$STATE_FILE")"
    if [[ "$run_tag" != ployz-wf779-* ]]; then
        printf 'refusing cleanup for unexpected run tag: %s\n' "$run_tag" >&2
        return 1
    fi

    local failed=0
    local instance_id
    while IFS= read -r instance_id; do
        if validate_instance_for_cleanup "$instance_id" "$run_tag"; then
            if api DELETE "/instances/$instance_id" >/dev/null; then
                printf 'deleted instance %s\n' "$instance_id"
            else
                failed=1
            fi
        else
            local validate_status=$?
            if [[ "$validate_status" -ne 2 ]]; then
                failed=1
            fi
        fi
    done < <(jq -r '.instances[].id' "$STATE_FILE")

    local ssh_key_id
    ssh_key_id="$(jq -r '.ssh_key_id // empty' "$STATE_FILE")"
    if [[ -n "$ssh_key_id" ]]; then
        local key_file
        key_file="$(mktemp "$RUN_DIR/ssh-key.XXXXXX")"
        local key_status
        key_status="$(curl --silent --show-error --output "$key_file" --write-out '%{http_code}' \
            --header "Authorization: Bearer $VULTR_API_KEY" \
            "$VULTR_API/ssh-keys/$ssh_key_id")"
        if [[ "$key_status" == "200" ]] && jq -e --arg name "$run_tag" \
            '.ssh_key.name == $name' "$key_file" >/dev/null; then
            if api DELETE "/ssh-keys/$ssh_key_id" >/dev/null; then
                printf 'deleted temporary SSH key %s\n' "$ssh_key_id"
            else
                failed=1
            fi
        elif [[ "$key_status" != "404" ]]; then
            printf 'refusing to delete SSH key %s without matching run name\n' "$ssh_key_id" >&2
            failed=1
        fi
        rm -f "$key_file"
    fi

    return "$failed"
}

cleanup_on_exit() {
    local status=$?
    trap - EXIT INT TERM
    stop_tunnels
    if [[ "$CLEANUP_ARMED" == true ]]; then
        if ! cleanup_run "$RUN_DIR"; then
            printf 'automatic cleanup failed; retry: %s cleanup %s\n' "$0" "$RUN_DIR" >&2
            status=1
        fi
    fi
    exit "$status"
}

keep_hosts_on_exit() {
    local status=$?
    trap - EXIT INT TERM
    stop_tunnels
    printf 'development hosts retained; resume: %s resume %s <phase>\n' \
        "$0" "$RUN_DIR" >&2
    exit "$status"
}

stop_tunnels() {
    local pid
    for pid in "${TUNNEL_PIDS[@]}"; do
        kill "$pid" 2>/dev/null || true
        wait "$pid" 2>/dev/null || true
    done
    TUNNEL_PIDS=()
}

start_tunnels() {
    stop_tunnels
    local node
    for node in 1 2 3; do
        ssh "${SSH_OPTIONS[@]}" \
            -N \
            -L "$((18080 + node)):127.0.0.1:8080" \
            "root@$(instance_ip "$node")" &
        TUNNEL_PIDS+=("$!")
    done
}

wait_for_api() {
    local node="$1"
    local token
    token="$(<"$RUN_DIR/token")"
    local attempt
    for attempt in $(seq 1 120); do
        if curl --silent --fail \
            --header "Authorization: Bearer $token" \
            --header "Content-Type: application/json" \
            --data '"SELECT 1"' \
            "http://127.0.0.1:$((18080 + node))/v1/queries" >/dev/null; then
            return
        fi
        sleep 1
    done
    printf 'Corrosion API did not become ready on node %s\n' "$node" >&2
    return 1
}

wait_for_all_apis() {
    local node
    for node in 1 2 3; do
        wait_for_api "$node"
    done
}

clock_residual_ms() {
    local node="$1"
    remote_shell "$node" \
        "chronyc tracking | awk '/System time/ { value = \$4 * 1000; if (\$5 == \"slow\") value = -value; print value }'"
}

collect_node() {
    local node="$1"
    local destination="$2"
    local pid
    pid="$(remote_command "$node" systemctl show --property MainPID --value corrosion.service)"
    remote_driver "$node" collect \
        --db-path /var/lib/corrosion/corrosion.db \
        --pid "$pid" >"$destination"
}

wait_for_cluster_convergence() {
    local phase="$1"
    local phase_dir="$RUN_DIR/evidence/$phase"
    mkdir -p "$phase_dir"
    local attempt
    for attempt in $(seq 1 90); do
        local node
        for node in 1 2 3; do
            collect_node "$node" "$phase_dir/node${node}-attempt${attempt}.json"
        done
        local vector1
        local vector2
        local vector3
        vector1="$(jq -cS .version_vector "$phase_dir/node1-attempt${attempt}.json")"
        vector2="$(jq -cS .version_vector "$phase_dir/node2-attempt${attempt}.json")"
        vector3="$(jq -cS .version_vector "$phase_dir/node3-attempt${attempt}.json")"
        if [[ "$vector1" == "$vector2" && "$vector1" == "$vector3" ]] &&
            jq -e '.gaps.missing_version_count == 0' \
                "$phase_dir/node1-attempt${attempt}.json" \
                "$phase_dir/node2-attempt${attempt}.json" \
                "$phase_dir/node3-attempt${attempt}.json" >/dev/null; then
            jq -n \
                --argjson attempt "$attempt" \
                --argjson version_vector "$vector1" \
                '{
                    command: "version-vector-barrier",
                    ok: true,
                    attempts: $attempt,
                    version_vector: $version_vector,
                    gaps: 0
                }' >"$phase_dir/barrier.json"
            return
        fi
        sleep 2
    done

    jq -n \
        '{command: "version-vector-barrier", ok: false, attempts: 90}' \
        >"$phase_dir/barrier.json"
    printf 'cluster did not converge during %s\n' "$phase" >&2
    return 1
}

prepare_remote_payloads() {
    local archive
    archive="$(<"$RUN_DIR/release-archive-path")"
    mkdir -p "$RUN_DIR/assets" "$RUN_DIR/secrets"
    tar -xzf "$archive" -C "$RUN_DIR/assets"
    chmod 0755 "$RUN_DIR/assets/corrosion"
    sha256sum "$RUN_DIR/assets/corrosion" >"$RUN_DIR/evidence/corrosion-binary.sha256"
    openssl rand -hex 32 >"$RUN_DIR/token"
    chmod 0600 "$RUN_DIR/token"

    local token
    token="$(<"$RUN_DIR/token")"
    local node
    for node in 1 2 3; do
        local values="$RUN_DIR/secrets/node${node}-values.json"
        jq -n \
            --arg gossip_addr "10.77.0.${node}:8787" \
            --arg bearer_token "$token" \
            --argjson node "$node" \
            '{
                db_path: "/var/lib/corrosion/corrosion.db",
                subscriptions_path: "/var/lib/corrosion/subscriptions",
                schema_path: "/etc/corrosion/schema",
                gossip_addr: $gossip_addr,
                bootstrap_peers: [1, 2, 3]
                    | map(select(. != $node) | "10.77.0.\(.):8787"),
                api_addr: "127.0.0.1:8080",
                bearer_token: $bearer_token,
                admin_socket: "/run/corrosion/admin.sock",
                test_reaper: {
                    check_interval: 5,
                    tables: {machine_facts: "30s"}
                }
            }' >"$values"
        chmod 0600 "$values"
    done
}

upload_and_install() {
    local binary_sha
    binary_sha="$(cut -d ' ' -f1 "$RUN_DIR/evidence/corrosion-binary.sha256")"

    upload_and_install_node() {
        local node="$1"
        remote_command "$node" mkdir -p \
            /root/ployz-spike/remote \
            /root/ployz-spike/schema
        scp_to_node "$SCRIPT_DIR/driver.py" "$node" /root/ployz-spike/driver.py
        scp_to_node "$RUN_DIR/token" "$node" /root/ployz-spike/token
        scp_to_node "$RUN_DIR/secrets/node${node}-values.json" \
            "$node" /root/ployz-spike/values.json
        scp_to_node "$SCRIPT_DIR/schema/v1.sql" \
            "$node" /root/ployz-spike/schema/schema.sql
        scp_to_node "$SCRIPT_DIR/schema/v2.sql" \
            "$node" /root/ployz-spike/schema-v2.sql
        local remote_file
        for remote_file in bootstrap.sh mesh.sh config.toml.tpl corrosion.service; do
            scp_to_node "$SCRIPT_DIR/remote/$remote_file" \
                "$node" "/root/ployz-spike/remote/$remote_file"
        done
        remote_command "$node" chmod 0600 \
            /root/ployz-spike/token \
            /root/ployz-spike/values.json
        remote_command "$node" curl \
            --fail \
            --location \
            --retry 3 \
            --output /root/ployz-spike/corrosion.tar.gz \
            "$RELEASE_URL"
        remote_shell "$node" \
            "printf '%s  %s\\n' '$RELEASE_SHA256' /root/ployz-spike/corrosion.tar.gz |
                sha256sum --check &&
             tar -xzf /root/ployz-spike/corrosion.tar.gz -C /root/ployz-spike"
        remote_command "$node" \
            /root/ployz-spike/remote/bootstrap.sh install \
            /root/ployz-spike/corrosion \
            "$binary_sha" \
            "$RELEASE_TAG" \
            "$RELEASE_EMBEDDED_VERSION" \
            /root/ployz-spike/values.json \
            /root/ployz-spike/schema
    }

    local -a install_pids=()
    local node
    for node in 1 2 3; do
        upload_and_install_node "$node" \
            >"$RUN_DIR/evidence/bootstrap-node${node}.txt" 2>&1 &
        install_pids+=("$!")
    done
    local pid
    local failed=0
    for pid in "${install_pids[@]}"; do
        if ! wait "$pid"; then
            failed=1
        fi
    done
    return "$failed"
}

configure_mesh() {
    local -a public_keys=()
    local node
    for node in 1 2 3; do
        public_keys[node]="$(remote_shell "$node" \
            'umask 077; wg genkey | tee /root/ployz-spike/wg-private.key | wg pubkey')"
    done

    for node in 1 2 3; do
        local -a peers=()
        local peer
        for peer in 1 2 3; do
            if [[ "$peer" -ne "$node" ]]; then
                peers+=(
                    "${public_keys[$peer]}|$(instance_ip "$peer"):51820|10.77.0.${peer}/32"
                )
            fi
        done
        remote_command "$node" \
            /root/ployz-spike/remote/mesh.sh \
            "10.77.0.${node}/24" \
            10.77.0.0/24 \
            /root/ployz-spike/wg-private.key \
            51820 \
            "${peers[@]}" \
            >"$RUN_DIR/evidence/mesh-node${node}.txt"
    done

    local -a clock_pids=()
    for node in 1 2 3; do
        remote_command "$node" /root/ployz-spike/remote/bootstrap.sh clock-gate \
            >"$RUN_DIR/evidence/clock-gate-node${node}.txt" 2>&1 &
        clock_pids+=("$!")
    done
    local pid
    local failed=0
    for pid in "${clock_pids[@]}"; do
        if ! wait "$pid"; then
            failed=1
        fi
    done
    if [[ "$failed" -ne 0 ]]; then
        return "$failed"
    fi

    local -a start_pids=()
    for node in 1 2 3; do
        remote_command "$node" /root/ployz-spike/remote/bootstrap.sh start \
            >"$RUN_DIR/evidence/corrosion-start-node${node}.txt" 2>&1 &
        start_pids+=("$!")
    done
    for pid in "${start_pids[@]}"; do
        if ! wait "$pid"; then
            failed=1
        fi
    done
    return "$failed"
}

wait_for_membership() {
    local phase_dir="$RUN_DIR/evidence/membership"
    mkdir -p "$phase_dir"
    local node
    for node in 1 2 3; do
        local peer=$((node % 3 + 1))
        remote_shell "$node" \
            "systemctl --no-pager --full status corrosion.service;
             wg show;
             ip -4 route show;
             ping -c 1 -W 2 10.77.0.$peer;
             ss -lunp;
             free -m;
             iptables-save;
             journalctl --unit corrosion.service --since '-2 minutes' --no-pager" \
            >"$phase_dir/node${node}-initial-diagnostics.txt" 2>&1
    done
    local attempt
    for attempt in $(seq 1 60); do
        local complete=0
        for node in 1 2 3; do
            remote_command "$node" \
                /usr/local/bin/corrosion cluster members \
                --config /etc/corrosion/config.toml \
                >"$phase_dir/node${node}-attempt${attempt}.txt"
            local members
            members="$(grep -c '^  \"id\"' "$phase_dir/node${node}-attempt${attempt}.txt" || true)"
            if [[ "$members" -eq 2 ]]; then
                complete=$((complete + 1))
            fi
        done
        if [[ "$complete" -eq 3 ]]; then
            jq -n \
                --argjson attempt "$attempt" \
                '{command: "membership-barrier", ok: true, attempts: $attempt, peers_per_node: 2}' \
                >"$phase_dir/barrier.json"
            return
        fi
        sleep 1
    done
    jq -n \
        '{command: "membership-barrier", ok: false, attempts: 60}' \
        >"$phase_dir/barrier.json"
    printf 'Corrosion membership did not form\n' >&2
    return 1
}

capture_initial_state() {
    local node
    for node in 1 2 3; do
        remote_command "$node" chronyc tracking \
            >"$RUN_DIR/evidence/clock-node${node}.txt"
        remote_command "$node" wg show \
            >"$RUN_DIR/evidence/wireguard-node${node}.txt"
        remote_command "$node" \
            /usr/local/bin/corrosion cluster members \
            --config /etc/corrosion/config.toml \
            >"$RUN_DIR/evidence/cluster-members-node${node}.txt"
        remote_command "$node" /usr/local/bin/corrosion --version \
            >"$RUN_DIR/evidence/version-node${node}.txt"
        collect_node "$node" "$RUN_DIR/evidence/initial-node${node}.json"
    done
}

run_idle_latency() {
    local phase_dir="$RUN_DIR/evidence/latency-idle"
    mkdir -p "$phase_dir"
    local writer_residual
    local observer_residual
    writer_residual="$(clock_residual_ms 1)"
    observer_residual="$(clock_residual_ms 2)"
    local sample
    for sample in $(seq 1 30); do
        local identifier
        identifier="idle-$(jq -r .run_id "$STATE_FILE")-$sample"
        local_driver 2 observe \
            --table machine_facts \
            --expected-id "$identifier" \
            --wait-seconds 20 \
            --poll-interval-ms 20 \
            --writer-residual-ms "$writer_residual" \
            --observer-residual-ms "$observer_residual" \
            >"$phase_dir/observe-$sample.json" &
        local observer_pid=$!
        sleep 0.05
        local_driver 1 write-idle \
            --writer-id node1 \
            --machine-id machine1 \
            --count 1 \
            --id "$identifier" \
            >"$phase_dir/write-$sample.json"
        wait "$observer_pid"
    done
}

run_deploy_latency() {
    local phase_dir="$RUN_DIR/evidence/latency-deploy"
    mkdir -p "$phase_dir"
    remote_driver 3 write-deploy \
        --writer-id node3-load \
        --machine-id machine3 \
        --bursts 200 \
        --interval-ms 2 \
        >"$phase_dir/background-load.json" &
    local load_pid=$!

    local writer_residual
    local observer_residual
    writer_residual="$(clock_residual_ms 1)"
    observer_residual="$(clock_residual_ms 2)"
    local sample
    for sample in $(seq 1 20); do
        local burst_id
        burst_id="deploy-$(jq -r .run_id "$STATE_FILE")-$sample"
        local_driver 2 observe \
            --table cluster_intent \
            --expected-id "${burst_id}-intent" \
            --wait-seconds 20 \
            --poll-interval-ms 20 \
            --writer-residual-ms "$writer_residual" \
            --observer-residual-ms "$observer_residual" \
            >"$phase_dir/observe-intent-$sample.json" &
        local intent_pid=$!
        local_driver 2 observe \
            --table route_state \
            --expected-id "${burst_id}-route" \
            --expected-id "${burst_id}-serving" \
            --wait-seconds 20 \
            --poll-interval-ms 20 \
            --writer-residual-ms "$writer_residual" \
            --observer-residual-ms "$observer_residual" \
            >"$phase_dir/observe-route-$sample.json" &
        local route_pid=$!
        local_driver 2 observe \
            --table machine_facts \
            --expected-id "${burst_id}-fact" \
            --wait-seconds 20 \
            --poll-interval-ms 20 \
            --writer-residual-ms "$writer_residual" \
            --observer-residual-ms "$observer_residual" \
            >"$phase_dir/observe-fact-$sample.json" &
        local fact_pid=$!
        sleep 0.05
        local_driver 1 write-deploy \
            --writer-id node1 \
            --machine-id machine1 \
            --bursts 1 \
            --burst-id "$burst_id" \
            >"$phase_dir/write-$sample.json"
        wait "$intent_pid"
        wait "$route_pid"
        wait "$fact_pid"
    done
    wait "$load_pid"
}

run_subscription_drills() {
    local phase_dir="$RUN_DIR/evidence/subscriptions"
    mkdir -p "$phase_dir"

    local_driver 2 subscription \
        --table machine_facts \
        --duration-seconds 15 \
        --max-changes 5 \
        >"$phase_dir/initial.json" &
    local subscription_pid=$!
    sleep 1
    local_driver 1 write-idle \
        --writer-id subscription \
        --machine-id machine1 \
        --count 5 \
        --interval-ms 20 \
        >"$phase_dir/initial-writes.json"
    wait "$subscription_pid"

    local query_id
    local last_change_id
    query_id="$(jq -er .query_id "$phase_dir/initial.json")"
    last_change_id="$(jq -er .last_change_id "$phase_dir/initial.json")"
    local_driver 2 subscription \
        --table machine_facts \
        --resume-id "$query_id" \
        --from-change-id "$last_change_id" \
        --duration-seconds 15 \
        --max-changes 5 \
        >"$phase_dir/resume.json" &
    subscription_pid=$!
    sleep 1
    local_driver 1 write-idle \
        --writer-id subscription-resume \
        --machine-id machine1 \
        --count 5 \
        --interval-ms 20 \
        >"$phase_dir/resume-writes.json"
    wait "$subscription_pid"

    remote_driver 1 write-idle \
        --writer-id subscription-window \
        --machine-id machine1 \
        --count 520 \
        --interval-ms 1 \
        >"$phase_dir/window-writes.json"
    local last_window_id
    last_window_id="$(jq -er '.ids[-1]' "$phase_dir/window-writes.json")"
    local_driver 2 observe \
        --table machine_facts \
        --expected-id "$last_window_id" \
        --wait-seconds 60 \
        --poll-interval-ms 20 \
        --writer-residual-ms 0 \
        --observer-residual-ms 0 \
        >"$phase_dir/window-before-resume.json"
    local resume_status=0
    if local_driver 2 subscription \
        --table machine_facts \
        --resume-id "$query_id" \
        --from-change-id "$last_change_id" \
        --duration-seconds 10 \
        --max-changes 1 \
        >"$phase_dir/window-resume.json"; then
        resume_status=0
    else
        resume_status=$?
    fi
    if [[ "$resume_status" -eq 0 ]] &&
        ! jq -e --argjson expected "$((last_change_id + 1))" \
            '.change_ids == [$expected]' "$phase_dir/window-resume.json" >/dev/null; then
        printf 'subscription window probe returned neither the next change nor a typed loss result\n' >&2
        return 1
    fi
    if [[ "$resume_status" -ne 0 ]] &&
        ! jq -e '.restart_resnapshot_required == true' \
            "$phase_dir/window-resume.json" >/dev/null; then
        printf 'subscription window probe failed without a typed resnapshot result\n' >&2
        return 1
    fi
    jq -n \
        --argjson resume_status "$resume_status" \
        --slurpfile result "$phase_dir/window-resume.json" \
        '{
            command: "subscription-window",
            ok: true,
            resume_exit_status: $resume_status,
            observed: $result[0],
            full_requery_required: $result[0].restart_resnapshot_required
        }' >"$phase_dir/window-summary.json"
    local_driver 2 observe \
        --table machine_facts \
        --expected-id "$last_window_id" \
        --wait-seconds 20 \
        --poll-interval-ms 20 \
        --writer-residual-ms 0 \
        --observer-residual-ms 0 \
        >"$phase_dir/window-full-requery.json"

    local_driver 3 subscription \
        --table machine_facts \
        --duration-seconds 40 \
        --max-changes 10 \
        >"$phase_dir/partition-stream.json" &
    subscription_pid=$!
    sleep 1
    remote_shell 3 \
        'iptables -w -I INPUT 1 -i wg0 -j DROP; iptables -w -I OUTPUT 1 -o wg0 -j DROP'
    local_driver 1 write-idle \
        --writer-id partition \
        --machine-id machine1 \
        --count 10 \
        --interval-ms 20 \
        >"$phase_dir/partition-writes.json"
    sleep 5
    remote_shell 3 \
        'iptables -w -D INPUT -i wg0 -j DROP; iptables -w -D OUTPUT -o wg0 -j DROP'
    wait "$subscription_pid"
    local -a expected_args=()
    local identifier
    while IFS= read -r identifier; do
        expected_args+=(--expected-id "$identifier")
    done < <(jq -r '.ids[]' "$phase_dir/partition-writes.json")
    local_driver 3 observe \
        --table machine_facts \
        "${expected_args[@]}" \
        --wait-seconds 30 \
        --poll-interval-ms 20 \
        --writer-residual-ms 0 \
        --observer-residual-ms 0 \
        >"$phase_dir/partition-full-requery.json"
}

run_disk_and_reaper_drill() {
    local phase_dir="$RUN_DIR/evidence/storage"
    mkdir -p "$phase_dir"
    local day
    for day in $(seq 1 7); do
        remote_driver 1 write-deploy \
            --writer-id "simulated-day-$day" \
            --machine-id machine1 \
            --bursts 100 \
            --interval-ms 1 \
            >"$phase_dir/day${day}-writes.json"
        collect_node 1 "$phase_dir/day${day}-node1.json"
    done
    local_driver 1 churn \
        --writer-id churn \
        --machine-id machine1 \
        --interval-ms 20 \
        >"$phase_dir/churn.json"
    collect_node 1 "$phase_dir/churn-before-reaper.json"
    sleep 40
    collect_node 1 "$phase_dir/churn-after-reaper.json"
    remote_command 1 journalctl \
        --unit corrosion.service \
        --since '-2 minutes' \
        --no-pager \
        >"$phase_dir/reaper-journal.txt"
}

run_recovery_drills() {
    local phase_dir="$RUN_DIR/evidence/recovery"
    mkdir -p "$phase_dir"
    local before_pid
    local before_restarts
    before_pid="$(remote_command 1 systemctl show --property MainPID --value corrosion.service)"
    before_restarts="$(remote_command 1 systemctl show --property NRestarts --value corrosion.service)"
    remote_command 1 systemctl kill \
        --kill-whom main \
        --signal KILL \
        corrosion.service
    wait_for_api 1
    local after_pid
    local after_restarts
    after_pid="$(remote_command 1 systemctl show --property MainPID --value corrosion.service)"
    after_restarts="$(remote_command 1 systemctl show --property NRestarts --value corrosion.service)"
    jq -n \
        --arg before_pid "$before_pid" \
        --arg after_pid "$after_pid" \
        --arg before_restarts "$before_restarts" \
        --arg after_restarts "$after_restarts" \
        '{
            command: "agent-crash-restart",
            ok: ($before_pid != $after_pid),
            before_pid: $before_pid,
            after_pid: $after_pid,
            before_restarts: $before_restarts,
            after_restarts: $after_restarts,
            operator_steps: []
        }' >"$phase_dir/crash.json"

    stop_tunnels
    remote_command 2 systemctl reboot || true
    sleep 5
    local attempt
    for attempt in $(seq 1 120); do
        if ssh_node 2 true 2>/dev/null; then
            break
        fi
        if [[ "$attempt" -eq 120 ]]; then
            printf 'node 2 did not return after reboot\n' >&2
            return 1
        fi
        sleep 2
    done
    start_tunnels
    wait_for_all_apis
    remote_command 2 systemctl is-active wg-quick@wg0.service \
        >"$phase_dir/reboot-wireguard.txt"
    remote_command 2 systemctl is-active corrosion.service \
        >"$phase_dir/reboot-corrosion.txt"
    jq -n \
        '{
            command: "cold-boot",
            ok: true,
            operator_steps: []
        }' >"$phase_dir/reboot.json"
}

run_schema_skew_drill() {
    local phase_dir="$RUN_DIR/evidence/schema-skew"
    mkdir -p "$phase_dir"
    local node
    for node in 1 2; do
        remote_command "$node" install \
            -o root -g corrosion -m 0640 \
            /root/ployz-spike/schema-v2.sql \
            /etc/corrosion/schema/schema.sql
        remote_command "$node" /usr/local/bin/corrosion reload \
            --config /etc/corrosion/config.toml \
            >"$phase_dir/reload-node${node}.txt"
    done

    local schema_id
    schema_id="schema-generation-$(jq -r .run_id "$STATE_FILE")"
    local_driver 1 write-idle \
        --writer-id schema-v2 \
        --machine-id machine1 \
        --count 1 \
        --id "$schema_id" \
        --schema-generation 2 \
        >"$phase_dir/write.json"
    local_driver 2 observe \
        --table machine_facts \
        --expected-id "$schema_id" \
        --wait-seconds 20 \
        --poll-interval-ms 20 \
        --writer-residual-ms 0 \
        --observer-residual-ms 0 \
        >"$phase_dir/node2-observe.json"
    if local_driver 3 observe \
        --table machine_facts \
        --expected-id "$schema_id" \
        --wait-seconds 5 \
        --poll-interval-ms 50 \
        --writer-residual-ms 0 \
        --observer-residual-ms 0 \
        >"$phase_dir/node3-stalled.json"; then
        printf 'schema-skew node unexpectedly applied the new-column write\n' >&2
        return 1
    fi
    remote_command 3 journalctl \
        --unit corrosion.service \
        --since '-2 minutes' \
        --no-pager \
        >"$phase_dir/node3-stall-journal.txt"
    remote_command 3 install \
        -o root -g corrosion -m 0640 \
        /root/ployz-spike/schema-v2.sql \
        /etc/corrosion/schema/schema.sql
    remote_command 3 /usr/local/bin/corrosion reload \
        --config /etc/corrosion/config.toml \
        >"$phase_dir/reload-node3.txt"

    finish_schema_heal_drill
}

finish_schema_heal_drill() {
    local phase_dir="$RUN_DIR/evidence/schema-skew"
    mkdir -p "$phase_dir"
    local schema_id
    schema_id="schema-generation-$(jq -r .run_id "$STATE_FILE")"
    local reload_only=true
    if [[ -f "$phase_dir/node3-healed.json" ]] &&
        ! jq -e '.ok == true' "$phase_dir/node3-healed.json" >/dev/null; then
        reload_only=false
    elif ! local_driver 3 observe \
        --table machine_facts \
        --expected-id "$schema_id" \
        --wait-seconds 10 \
        --poll-interval-ms 20 \
        --writer-residual-ms 0 \
        --observer-residual-ms 0 \
        >"$phase_dir/node3-healed.json"; then
        reload_only=false
    fi

    if [[ "$reload_only" != true ]]; then
        if [[ ! -f "$phase_dir/node3-after-restart.json" ]] ||
            ! jq -e '.ok == true' "$phase_dir/node3-after-restart.json" >/dev/null; then
            remote_command 3 systemctl restart corrosion.service
            wait_for_api 3
            local_driver 3 observe \
                --table machine_facts \
                --expected-id "$schema_id" \
                --wait-seconds 30 \
                --poll-interval-ms 20 \
                --writer-residual-ms 0 \
                --observer-residual-ms 0 \
                >"$phase_dir/node3-after-restart.json"
        fi
    fi

    jq -n \
        --argjson reload_only "$reload_only" \
        '{
            command: "heal-schema-skew",
            ok: true,
            reload_only: $reload_only,
            operator_steps: (
                if $reload_only then
                    [{command: "install schema and reload", category: "recovery"}]
                else
                    [
                        {command: "install schema and reload", category: "recovery"},
                        {command: "restart the stalled agent once", category: "recovery"}
                    ]
                end
            )
        }' >"$phase_dir/summary.json"
    wait_for_cluster_convergence schema-heal
}

run_same_version_replacement() {
    local phase_dir="$RUN_DIR/evidence/version-replacement"
    mkdir -p "$phase_dir"
    local binary_sha
    binary_sha="$(cut -d ' ' -f1 "$RUN_DIR/evidence/corrosion-binary.sha256")"
    local node
    for node in 1 2 3; do
        remote_shell "$node" \
            "install -o root -g root -m 0755 /root/ployz-spike/corrosion /usr/local/bin/corrosion.next &&
             printf '%s  %s\\n' '$binary_sha' /usr/local/bin/corrosion.next | sha256sum --check &&
             mv /usr/local/bin/corrosion.next /usr/local/bin/corrosion &&
             systemctl restart corrosion.service"
        wait_for_api "$node"
        remote_command "$node" /usr/local/bin/corrosion --version \
            >"$phase_dir/node${node}-version.txt"
        collect_node "$node" "$phase_dir/node${node}.json"
    done
    jq -n \
        --arg release_tag "$RELEASE_TAG" \
        --arg embedded_version "$RELEASE_EMBEDDED_VERSION" \
        '{
            command: "same-version-atomic-replacement",
            ok: true,
            limitations: [
                ($release_tag + " is the only published stable v1 asset, so no adjacent-version rolling upgrade was invented."),
                ("The official " + $release_tag + " asset embeds the version string " + $embedded_version + ".")
            ],
            operator_steps: [
                {
                    command: "replace one verified binary, restart that node, verify it, then continue",
                    category: "recovery"
                }
            ]
        }' >"$phase_dir/summary.json"
}

run_reseed_drill() {
    local phase_dir="$RUN_DIR/evidence/reseed"
    mkdir -p "$phase_dir"
    local before_id
    local after_id
    before_id="reseed-before-$(jq -r .run_id "$STATE_FILE")"
    after_id="reseed-after-$(jq -r .run_id "$STATE_FILE")"
    local_driver 1 write-idle \
        --writer-id reseed \
        --machine-id machine1 \
        --count 1 \
        --id "$before_id" \
        >"$phase_dir/before-snapshot-write.json"
    local_driver 3 observe \
        --table machine_facts \
        --expected-id "$before_id" \
        --wait-seconds 30 \
        --poll-interval-ms 20 \
        --writer-residual-ms 0 \
        --observer-residual-ms 0 \
        >"$phase_dir/before-snapshot-observe.json"
    remote_command 1 /usr/local/bin/corrosion backup \
        --config /etc/corrosion/config.toml \
        /root/ployz-spike/snapshot.db \
        >"$phase_dir/backup.txt"
    local_driver 1 write-idle \
        --writer-id reseed-replay \
        --machine-id machine1 \
        --count 1 \
        --id "$after_id" \
        >"$phase_dir/after-snapshot-write.json"
    local_driver 3 observe \
        --table machine_facts \
        --expected-id "$after_id" \
        --wait-seconds 30 \
        --poll-interval-ms 20 \
        --writer-residual-ms 0 \
        --observer-residual-ms 0 \
        >"$phase_dir/after-snapshot-observe.json"

    scp_from_node 1 /root/ployz-spike/snapshot.db "$RUN_DIR/snapshot.db"
    sqlite3 "$RUN_DIR/snapshot.db" \
        "INSERT INTO __corro_state(key, value) VALUES ('cluster_id', 1)
         ON CONFLICT(key) DO UPDATE SET value = value + 1;"
    sha256sum "$RUN_DIR/snapshot.db" >"$phase_dir/snapshot.sha256"
    local snapshot_sha
    snapshot_sha="$(cut -d ' ' -f1 "$phase_dir/snapshot.sha256")"

    local node
    for node in 1 2 3; do
        scp_to_node "$RUN_DIR/snapshot.db" "$node" /root/ployz-spike/reseed.db
        remote_command "$node" systemctl stop corrosion.service
    done
    for node in 1 2 3; do
        remote_command "$node" /usr/local/bin/corrosion restore \
            --config /etc/corrosion/config.toml \
            /root/ployz-spike/reseed.db \
            >"$phase_dir/restore-node${node}.txt"
        remote_shell "$node" \
            "printf '%s\\n' '$snapshot_sha' > /var/lib/corrosion/reseed.marker &&
             chown corrosion:corrosion /var/lib/corrosion/reseed.marker &&
             chmod 0640 /var/lib/corrosion/reseed.marker"
    done

    finish_reseed_drill
}

finish_reseed_drill() {
    local phase_dir="$RUN_DIR/evidence/reseed"
    local before_id
    local after_id
    before_id="reseed-before-$(jq -r .run_id "$STATE_FILE")"
    after_id="reseed-after-$(jq -r .run_id "$STATE_FILE")"
    local snapshot_sha
    snapshot_sha="$(cut -d ' ' -f1 "$phase_dir/snapshot.sha256")"

    local -a start_pids=()
    local node
    for node in 1 2 3; do
        remote_shell "$node" \
            "chown corrosion:corrosion /var/lib/corrosion/corrosion.db* &&
             systemctl reset-failed corrosion.service &&
             systemctl start --no-block corrosion.service" &
        start_pids+=("$!")
    done
    local pid
    local failed=0
    for pid in "${start_pids[@]}"; do
        if ! wait "$pid"; then
            failed=1
        fi
    done
    if [[ "$failed" -ne 0 ]]; then
        printf 'failed to start one or more reseeded agents\n' >&2
        return 1
    fi
    wait_for_all_apis
    wait_for_cluster_convergence reseed-barrier

    local_driver 2 observe \
        --table machine_facts \
        --expected-id "$before_id" \
        --wait-seconds 10 \
        --poll-interval-ms 20 \
        --writer-residual-ms 0 \
        --observer-residual-ms 0 \
        >"$phase_dir/before-row-preserved.json"
    if local_driver 2 observe \
        --table machine_facts \
        --expected-id "$after_id" \
        --wait-seconds 2 \
        --poll-interval-ms 50 \
        --writer-residual-ms 0 \
        --observer-residual-ms 0 \
        >"$phase_dir/after-row-absent.json"; then
        printf 'post-snapshot row unexpectedly survived reseed\n' >&2
        return 1
    fi
    local_driver 1 write-idle \
        --writer-id reseed-replay \
        --machine-id machine1 \
        --count 1 \
        --id "$after_id" \
        >"$phase_dir/replay.json"
    local_driver 3 observe \
        --table machine_facts \
        --expected-id "$after_id" \
        --wait-seconds 30 \
        --poll-interval-ms 20 \
        --writer-residual-ms 0 \
        --observer-residual-ms 0 \
        >"$phase_dir/replay-observe.json"
    jq -n \
        --arg snapshot_sha "$snapshot_sha" \
        '{
            command: "v1-reseed",
            ok: true,
            snapshot_sha256: $snapshot_sha,
            operator_steps: [
                {command: "freeze or record writes", category: "recovery"},
                {command: "backup and bump cluster_id", category: "recovery"},
                {command: "stop all agents and restore every node", category: "recovery"},
                {command: "wait for version-vector and gap barrier", category: "recovery"},
                {command: "replay post-snapshot writes", category: "recovery"}
            ]
        }' >"$phase_dir/summary.json"
}

capture_debugging_evidence() {
    local phase_dir="$RUN_DIR/evidence/debugging"
    mkdir -p "$phase_dir"
    local node
    for node in 1 2; do
        remote_command "$node" sqlite3 -readonly -json \
            /var/lib/corrosion/corrosion.db \
            "SELECT kind, COUNT(*) AS rows FROM machine_facts GROUP BY kind ORDER BY kind;" \
            >"$phase_dir/sqlite-node${node}.json"
        remote_command "$node" journalctl \
            --unit corrosion.service \
            --since '-20 minutes' \
            --no-pager \
            >"$phase_dir/journal-node${node}.txt"
    done
}

sync_changed_payloads() {
    local node
    for node in 1 2 3; do
        scp_to_node "$SCRIPT_DIR/driver.py" "$node" /root/ployz-spike/driver.py
        scp_to_node "$SCRIPT_DIR/schema/v2.sql" \
            "$node" /root/ployz-spike/schema-v2.sql
    done
}

validate_retained_hosts() {
    require_api_key
    local run_tag
    run_tag="$(jq -er .run_tag "$STATE_FILE")"
    local node
    for node in 1 2 3; do
        local instance_id
        instance_id="$(jq -er --arg node "$node" \
            '.instances[] | select(.node == $node) | .id' "$STATE_FILE")"
        api GET "/instances/$instance_id" |
            jq -e --arg tag "$run_tag" \
                '.instance.status == "active" and
                 (.instance.tags | index($tag) != null)' >/dev/null
    done
}

resume_drills() {
    local start_phase="$1"
    local -a phases=(
        setup
        idle
        deploy
        subscriptions
        storage
        recovery
        schema
        schema-heal
        replacement
        reseed
        reseed-start
        debugging
        report
    )
    local known=false
    local phase
    for phase in "${phases[@]}"; do
        if [[ "$phase" == "$start_phase" ]]; then
            known=true
        fi
    done
    if [[ "$known" != true ]]; then
        printf 'unknown resume phase: %s\n' "$start_phase" >&2
        printf 'phases: %s\n' "${phases[*]}" >&2
        return 2
    fi

    if [[ "$start_phase" == setup ]]; then
        prepare_remote_payloads
        upload_and_install
        configure_mesh
        wait_for_membership
        start_tunnels
        wait_for_all_apis
        capture_initial_state
    else
        sync_changed_payloads
        start_tunnels
        if [[ "$start_phase" != reseed-start ]]; then
            wait_for_all_apis
        fi
    fi

    local running=false
    for phase in "${phases[@]}"; do
        if [[ "$phase" == "$start_phase" ]]; then
            running=true
        fi
        if [[ "$running" != true || "$phase" == setup ]]; then
            continue
        fi
        case "$phase" in
            idle) run_idle_latency ;;
            deploy) run_deploy_latency ;;
            subscriptions) run_subscription_drills ;;
            storage) run_disk_and_reaper_drill ;;
            recovery) run_recovery_drills ;;
            schema) run_schema_skew_drill ;;
            schema-heal)
                if [[ "$start_phase" == schema-heal ]]; then
                    finish_schema_heal_drill
                fi
                ;;
            replacement) run_same_version_replacement ;;
            reseed) run_reseed_drill ;;
            reseed-start)
                if [[ "$start_phase" == reseed-start ]]; then
                    finish_reseed_drill
                fi
                ;;
            debugging) capture_debugging_evidence ;;
            report)
                python3 "$SCRIPT_DIR/driver.py" report \
                    --evidence-dir "$RUN_DIR/evidence" \
                    >"$RUN_DIR/evidence/report.json"
                printf 'evidence report: %s\n' "$RUN_DIR/evidence/report.md"
                ;;
        esac
    done
}

resume_run() {
    local reuse_dir="$1"
    local start_phase="$2"
    RUN_DIR="$(cd -- "$reuse_dir" && pwd)"
    STATE_FILE="$RUN_DIR/state.json"
    if [[ ! -f "$STATE_FILE" || ! -f "$RUN_DIR/ssh_key" ]]; then
        printf 'retained run is missing state or SSH key: %s\n' "$RUN_DIR" >&2
        return 1
    fi
    require_command jq
    require_command python3
    load_release_manifest
    validate_retained_hosts
    wait_for_ssh
    trap keep_hosts_on_exit EXIT INT TERM
    resume_drills "$start_phase"
}

bootstrap_and_run_drills() {
    prepare_remote_payloads
    upload_and_install
    configure_mesh
    wait_for_membership
    start_tunnels
    wait_for_all_apis
    capture_initial_state

    run_idle_latency
    run_deploy_latency
    run_subscription_drills
    run_disk_and_reaper_drill
    run_recovery_drills
    run_schema_skew_drill
    run_same_version_replacement
    run_reseed_drill
    capture_debugging_evidence

    python3 "$SCRIPT_DIR/driver.py" report \
        --evidence-dir "$RUN_DIR/evidence" \
        >"$RUN_DIR/evidence/report.json"
    printf 'evidence report: %s\n' "$RUN_DIR/evidence/report.md"
}

run_spike() {
    umask 077
    local run_id
    run_id="$(date -u +%Y%m%dT%H%M%SZ)-$$"
    RUN_DIR="$RUNS_ROOT/$run_id"
    STATE_FILE="$RUN_DIR/state.json"
    mkdir -p "$RUN_DIR/evidence"
    jq -n \
        --arg run_id "$run_id" \
        --arg run_tag "ployz-wf779-$run_id" \
        --arg region "$REGION" \
        --arg plan "$PLAN" \
        --argjson os_id "$OS_ID" \
        '{
            run_id: $run_id,
            run_tag: $run_tag,
            region: $region,
            plan: $plan,
            os_id: $os_id,
            ssh_key_id: null,
            instances: []
        }' >"$STATE_FILE"
    printf 'run directory: %s\n' "$RUN_DIR"

    preflight
    local release_archive
    release_archive="$(download_release)"
    printf '%s\n' "$release_archive" >"$RUN_DIR/release-archive-path"

    trap cleanup_on_exit EXIT INT TERM
    CLEANUP_ARMED=true
    create_ssh_key
    create_instances
    wait_for_instances
    wait_for_ssh
    bootstrap_and_run_drills
}

case "${1:-run}" in
    run)
        if [[ "$#" -ne 0 ]]; then
            usage >&2
            exit 2
        fi
        run_spike
        ;;
    cleanup)
        if [[ "$#" -ne 2 ]]; then
            usage >&2
            exit 2
        fi
        cleanup_run "$2"
        ;;
    resume)
        if [[ "$#" -ne 3 ]]; then
            usage >&2
            exit 2
        fi
        resume_run "$2" "$3"
        ;;
    *)
        usage >&2
        exit 2
        ;;
esac
