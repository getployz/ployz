import { execFileSync } from "node:child_process";
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { describe, expect, it, vi } from "vitest";

const reap = resolve(import.meta.dirname, "../../scripts/verify/reap.sh");

/** A `docker` that serves containers from `<root>/containers/<name>/{checkout,created}` and records each removal. */
const FAKE_DOCKER = `#!/usr/bin/env bash
root=$FAKE_DOCKER_ROOT
case "$1" in
  ps) for dir in "$root"/containers/*/; do [ -f "$dir/checkout" ] && basename "$dir"; done ;;
  inspect)
    format=$3 container=$4
    case "$format" in
      *Labels*) cat "$root/containers/$container/checkout" ;;
      *Created*) cat "$root/containers/$container/created" ;;
    esac ;;
  rm) shift; for container in "$@"; do [ "$container" = --force ] && continue; rm -rf "\${root:?}/containers/$container"; echo "$container" >> "$root/removed"; done ;;
esac
`;

function host() {
  const root = mkdtempSync(join(tmpdir(), "verify-reap-"));
  mkdirSync(join(root, "bin"));
  writeFileSync(join(root, "bin", "docker"), FAKE_DOCKER);
  chmodSync(join(root, "bin", "docker"), 0o755);
  mkdirSync(join(root, "containers"));
  const container = (name: string, checkout: string | null, createdSecondsAgo: number) => {
    mkdirSync(join(root, "containers", name));
    if (checkout !== null) writeFileSync(join(root, "containers", name, "checkout"), checkout);
    writeFileSync(join(root, "containers", name, "created"), new Date(Date.now() - createdSecondsAgo * 1000).toISOString());
  };
  /** A checkout whose verify run records `pids`, with an idle.sh that writes `idle-started` when run. */
  const checkout = (name: string, pids: { readonly vite?: number; readonly idle?: number }) => {
    const wt = join(root, name);
    const run = join(wt, "dashboard", ".verify", "run");
    mkdirSync(run, { recursive: true });
    mkdirSync(join(wt, "dashboard", "scripts", "verify"), { recursive: true });
    writeFileSync(join(wt, "dashboard", "scripts", "verify", "idle.sh"), `#!/usr/bin/env bash\necho "$1" > "${run}/idle-started"\n`);
    chmodSync(join(wt, "dashboard", "scripts", "verify", "idle.sh"), 0o755);
    writeFileSync(join(run, "env"), "PORT=4321\n");
    if (pids.vite !== undefined) writeFileSync(join(run, "vite.pid"), `${pids.vite}\n`);
    if (pids.idle !== undefined) writeFileSync(join(run, "idle.pid"), `${pids.idle}\n`);
    return { wt, run };
  };
  const run = () => execFileSync(reap, [], {
    encoding: "utf8",
    env: { ...process.env, PATH: `${join(root, "bin")}:${process.env.PATH ?? ""}`, FAKE_DOCKER_ROOT: root },
  });
  const removed = () => existsSync(join(root, "removed")) ? readFileSync(join(root, "removed"), "utf8").trim().split("\n").sort() : [];
  return { root, container, checkout, run, removed };
}

/** A pid that no process has: the highest Linux allows. */
const DEAD = 4_194_304;

describe("scripts/verify/reap.sh", () => {
  it("removes the containers of a checkout that is gone", () => {
    const docker = host();
    docker.container("ployz-verify-aaaaaaaaaa", join(docker.root, "deleted-worktree"), 30);
    docker.container("ployz-verify-aaaaaaaaaa-mailpit", join(docker.root, "deleted-worktree"), 30);

    const output = docker.run();

    expect(docker.removed()).toEqual(["ployz-verify-aaaaaaaaaa", "ployz-verify-aaaaaaaaaa-mailpit"]);
    expect(output).toContain("is gone");
  });

  it("removes the containers of a checkout whose vite has stopped, once past the grace", () => {
    const docker = host();
    const { wt } = docker.checkout("stopped", { vite: DEAD, idle: DEAD });
    docker.container("ployz-verify-bbbbbbbbbb", wt, 3600);

    docker.run();

    expect(docker.removed()).toEqual(["ployz-verify-bbbbbbbbbb"]);
  });

  it("keeps a container an instance may still be starting", () => {
    const docker = host();
    const { wt } = docker.checkout("starting", {});
    docker.container("ployz-verify-cccccccccc", wt, 30);

    docker.run();

    expect(docker.removed()).toEqual([]);
  });

  it("keeps a live instance and starts its watchdog again when it died", async () => {
    const docker = host();
    const { wt, run } = docker.checkout("live", { vite: process.pid, idle: DEAD });
    docker.container("ployz-verify-dddddddddd", wt, 3600);

    const output = docker.run();

    expect(docker.removed()).toEqual([]);
    expect(output).toContain("watchdog");
    await vi.waitFor(() => expect(readFileSync(join(run, "idle-started"), "utf8").trim()).toBe("4321"));
  });

  it("leaves a container no checkout labelled alone", () => {
    const docker = host();
    docker.container("ployz-verify-eeeeeeeeee", null, 86_400);

    docker.run();

    expect(docker.removed()).toEqual([]);
  });

  it("runs before up.sh starts its own containers", () => {
    const up = readFileSync(resolve(import.meta.dirname, "../../scripts/verify/up.sh"), "utf8");

    expect(up.indexOf("scripts/verify/reap.sh")).toBeGreaterThan(-1);
    expect(up.indexOf("scripts/verify/reap.sh")).toBeLessThan(up.indexOf("docker run"));
  });
});
