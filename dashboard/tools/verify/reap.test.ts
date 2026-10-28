import { execFileSync, spawn } from "node:child_process";
import { once } from "node:events";
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { afterEach, describe, expect, it } from "vitest";

const reap = resolve(import.meta.dirname, "../../scripts/verify/reap.sh");

/**
 * A `docker` that serves containers from `<root>/containers/<id>/{name,checkout,created}`, lists only labelled ones
 * when `ps` filters by the label, and records each id it removes.
 */
const FAKE_DOCKER = `#!/usr/bin/env bash
root=$FAKE_DOCKER_ROOT
case "$1" in
  ps)
    labelled=0; for arg in "$@"; do [ "$arg" = label=ployz.verify.checkout ] && labelled=1; done
    for dir in "$root"/containers/*/; do
      [ -d "$dir" ] || continue
      if [ "$labelled" = 0 ] || [ -f "$dir/checkout" ]; then basename "$dir"; fi
    done ;;
  inspect)
    format=$3 dir=$root/containers/$4
    case "$format" in
      *Labels*) cat "$dir/checkout" 2>/dev/null || echo ;;
      *Created*) cat "$dir/created" ;;
      *Name*) echo "/$(cat "$dir/name")" ;;
    esac ;;
  rm) shift; for id in "$@"; do [ "$id" = --force ] && continue; rm -rf "\${root:?}/containers/$id"; echo "$id" >> "$root/removed"; done ;;
esac
`;

const children: { kill: () => void }[] = [];
afterEach(() => {
  for (const child of children.splice(0)) child.kill();
});

/** A live process whose command line is vite's, running in `dir` as up.sh starts it there. */
function vite(dir: string): number {
  const child = spawn(process.execPath, ["-e", "setInterval(() => {}, 1000)", "node_modules/vite/bin/vite.js", "dev"], {
    cwd: dir,
    stdio: "ignore",
  });
  children.push(child);
  if (child.pid === undefined) throw new Error("vite stand-in did not start");
  return child.pid;
}

/** A live vite stand-in in `dir` that listens on a free port, as a vite restarted by hand would, outside vite.pid. */
async function listeningVite(dir: string): Promise<number> {
  const child = spawn(
    process.execPath,
    ["-e", "const s=require('net').createServer().listen(0,'127.0.0.1',()=>console.log(s.address().port))", "node_modules/vite/bin/vite.js", "dev"],
    { cwd: dir, stdio: ["ignore", "pipe", "ignore"] },
  );
  children.push(child);
  const [line] = (await once(child.stdout, "data")) as [Buffer];
  return Number(line.toString().trim());
}

function host() {
  const root = mkdtempSync(join(tmpdir(), "verify-reap-"));
  mkdirSync(join(root, "bin"));
  writeFileSync(join(root, "bin", "docker"), FAKE_DOCKER);
  chmodSync(join(root, "bin", "docker"), 0o755);
  mkdirSync(join(root, "containers"));
  const container = (id: string, checkout: string | null, createdSecondsAgo: number) => {
    const dir = join(root, "containers", id);
    mkdirSync(dir);
    writeFileSync(join(dir, "name"), `ployz-verify-${id}`);
    if (checkout !== null) writeFileSync(join(dir, "checkout"), checkout);
    writeFileSync(join(dir, "created"), new Date(Date.now() - createdSecondsAgo * 1000).toISOString());
  };
  /** A checkout whose verify run records `pids`, with an idle.sh that writes `idle-started` if anything runs it. */
  const checkout = (
    name: string,
    run: { readonly vite?: number; readonly live?: true; readonly idle?: number; readonly port?: number },
  ) => {
    const wt = join(root, name);
    const dir = join(wt, "dashboard", ".verify", "run");
    mkdirSync(dir, { recursive: true });
    mkdirSync(join(wt, "dashboard", "scripts", "verify"), { recursive: true });
    writeFileSync(
      join(wt, "dashboard", "scripts", "verify", "idle.sh"),
      `#!/usr/bin/env bash\ntouch "${dir}/idle-started"\n`,
    );
    chmodSync(join(wt, "dashboard", "scripts", "verify", "idle.sh"), 0o755);
    writeFileSync(join(dir, "env"), `PORT=${run.port ?? 4321}\n`);
    if (run.vite !== undefined) writeFileSync(join(dir, "vite.pid"), `${run.vite}\n`);
    if (run.idle !== undefined) writeFileSync(join(dir, "idle.pid"), `${run.idle}\n`);
    if (run.live === true) writeFileSync(join(dir, "vite.pid"), `${vite(join(wt, "dashboard"))}\n`);
    return { wt, run: dir };
  };
  const run = (env: Record<string, string> = {}) => execFileSync(reap, [], {
    encoding: "utf8",
    env: { ...process.env, ...env, PATH: `${join(root, "bin")}:${process.env["PATH"] ?? ""}`, FAKE_DOCKER_ROOT: root },
  });
  const removed = () => existsSync(join(root, "removed")) ? readFileSync(join(root, "removed"), "utf8").trim().split("\n").sort() : [];
  return { root, container, checkout, run, removed };
}

/** A pid that no process has: the highest Linux allows. */
const DEAD = 4_194_304;

describe("scripts/verify/reap.sh", () => {
  it("removes, by id, the containers of a checkout that is gone once past the grace", () => {
    const docker = host();
    docker.container("aaaaaaaaaa", join(docker.root, "deleted-worktree"), 3600);
    docker.container("aaaaaaaaab", join(docker.root, "deleted-worktree"), 3600);

    const output = docker.run();

    expect(docker.removed()).toEqual(["aaaaaaaaaa", "aaaaaaaaab"]);
    expect(output).toContain("reap: removed ployz-verify-aaaaaaaaaa (its checkout");
  });

  it("keeps a young container whose checkout is gone, and one whose checkout's parent this host cannot see", () => {
    const docker = host();
    docker.container("aaaaaaaaac", join(docker.root, "deleted-worktree"), 30);
    docker.container("aaaaaaaaad", join(docker.root, "unmounted", "worktree"), 3600);

    docker.run();

    expect(docker.removed()).toEqual([]);
  });

  it("removes the containers of a checkout whose vite has stopped, once past the grace", () => {
    const docker = host();
    const { wt } = docker.checkout("stopped", { vite: DEAD, idle: DEAD });
    docker.container("bbbbbbbbbb", wt, 3600);

    docker.run();

    expect(docker.removed()).toEqual(["bbbbbbbbbb"]);
  });

  it("removes the containers of a checkout whose vite pid another process now has", () => {
    const docker = host();
    const { wt } = docker.checkout("reused", { vite: process.pid });
    docker.container("bbbbbbbbbc", wt, 3600);

    docker.run();

    expect(docker.removed()).toEqual(["bbbbbbbbbc"]);
  });

  it("removes the containers of a checkout whose vite pid another checkout's vite now has", () => {
    const docker = host();
    const other = docker.checkout("other", { live: true });
    const { wt } = docker.checkout("stale", { vite: Number(readFileSync(join(other.run, "vite.pid"), "utf8")), idle: DEAD });
    docker.container("bbbbbbbbbd", wt, 3600);

    const output = docker.run();

    expect(docker.removed()).toEqual(["bbbbbbbbbd"]);
    expect(output).not.toContain("watchdog");
  });

  it("keeps the containers of a checkout whose vite.pid is stale while its own vite listens on its port", async () => {
    const docker = host();
    const wt = join(docker.root, "restarted");
    mkdirSync(join(wt, "dashboard"), { recursive: true });
    const port = await listeningVite(join(wt, "dashboard"));
    docker.checkout("restarted", { vite: DEAD, port });
    docker.container("bbbbbbbbbf", wt, 3600);

    docker.run();

    expect(docker.removed()).toEqual([]);
  });

  it("removes the containers of a checkout whose port another checkout's vite listens on", async () => {
    const docker = host();
    const other = join(docker.root, "other");
    mkdirSync(join(other, "dashboard"), { recursive: true });
    const port = await listeningVite(join(other, "dashboard"));
    const { wt } = docker.checkout("taken", { vite: DEAD, port });
    docker.container("bbbbbbbbbg", wt, 3600);

    docker.run();

    expect(docker.removed()).toEqual(["bbbbbbbbbg"]);
  });

  it("keeps the containers of a checkout it may not enter", () => {
    const docker = host();
    const { wt } = docker.checkout("private", { vite: DEAD });
    docker.container("bbbbbbbbbe", wt, 3600);
    chmodSync(wt, 0o000);
    try {
      docker.run();
    } finally {
      chmodSync(wt, 0o755);
    }

    expect(docker.removed()).toEqual([]);
  });

  it("keeps a container an instance may still be starting", () => {
    const docker = host();
    const { wt } = docker.checkout("starting", {});
    docker.container("cccccccccc", wt, 30);

    docker.run();

    expect(docker.removed()).toEqual([]);
  });

  it("keeps a live instance whose watchdog died, names it, and starts no watchdog", () => {
    const docker = host();
    const { wt, run } = docker.checkout("live", { live: true, idle: DEAD });
    docker.container("dddddddddd", wt, 3600);

    const output = docker.run();

    expect(docker.removed()).toEqual([]);
    expect(output).toContain(`reap: ${wt} runs with no idle watchdog`);
    expect(existsSync(join(run, "idle-started"))).toBe(false);
  });

  it("only lists what it would remove when dry", () => {
    const docker = host();
    docker.container("ffffffffff", join(docker.root, "deleted-worktree"), 3600);

    const output = docker.run({ VERIFY_REAP_DRY: "1" });

    expect(docker.removed()).toEqual([]);
    expect(output).toContain("reap: would remove ployz-verify-ffffffffff");
  });

  it("leaves a container no checkout labelled alone", () => {
    const docker = host();
    docker.container("eeeeeeeeee", null, 86_400);

    docker.run();

    expect(docker.removed()).toEqual([]);
  });

  it("runs before up.sh starts its own containers", () => {
    const up = readFileSync(resolve(import.meta.dirname, "../../scripts/verify/up.sh"), "utf8");

    expect(up.indexOf("scripts/verify/reap.sh")).toBeGreaterThan(-1);
    expect(up.indexOf("scripts/verify/reap.sh")).toBeLessThan(up.indexOf("docker run"));
  });
});
