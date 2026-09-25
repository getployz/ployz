import { describe, expect, it } from "vitest";
import type { ContainerId, DeployOperation, MachineId, OperationRow } from "@ployz/sdk";
import { resolvedServiceSpecFixture } from "#/modules/runtime/runtime-watch-frame.test-fixture";
import { canonicalJson } from "#/modules/environment-design/canonical-json";
import { parseServiceConfig } from "@ployz/sdk/config";
import { builtOnLine, deploymentProgressForEvent, deploymentStatusLabel, deploymentView, type AttemptTargetNode, type DeploymentViewInput } from "./deployment-view";

function row(index: number, operation?: DeployOperation): OperationRow {
  const spec = resolvedServiceSpecFixture();
  spec.container.environment = { SECRET: "never-publish" };
  return { index, machine_id: `machine-${index}` as MachineId, machine_name: `server-${index}`, service_name: `svc-${index}`, display_name: `api-${index}`,
    operation: operation ?? { type: "run_container", machine_id: `machine-${index}` as MachineId, spec, skip_health_monitor: false }, status: { type: "pending" } };
}
/** The Engine serializes keys alphabetically, unlike the planned rows. */
const engineOrdered = <T,>(value: T): T => JSON.parse(canonicalJson(value)) as T;
const context = { serviceIdFor: (name: string | null) => name };
const step = (id: number, build: number, key: string, name: string, start: number, end: number | null, error: string | null = null, image: string | null = null) =>
  ({ id, image, build, key, name, startedAt: new Date(start * 1000), completedAt: end === null ? null : new Date(end * 1000), error });
const deployment = (status: DeploymentViewInput["deployment"]["status"], extra: Partial<DeploymentViewInput["deployment"]> = {}): DeploymentViewInput["deployment"] =>
  ({ status, failureMessage: null, deployPreview: null, ...extra });
/** A service in the Attempt Target; `image` makes it a built one. */
const node = ({ nodeId, changed, removed = false, image = null }: { nodeId: string; changed: boolean; removed?: boolean; image?: string | null }): AttemptTargetNode => ({
  nodeId, changed, removed, image, nodeType: "service",
  config: parseServiceConfig({ version: 2, source: { version: 1, type: "empty", rootDir: "/" }, healthcheck: { type: "none" }, restartPolicy: "unless-stopped", privateDns: nodeId }),
});

describe("deployment view projection", () => {
  it("retains concurrent phases, identities and health deadlines without resolved secrets", () => {
    const rows: OperationRow[] = [
      { ...row(0), status: { type: "completed" } },
      { ...row(1), status: { type: "running", phase: { type: "waiting_for_health", container_id: "container-1" as ContainerId, health: "starting", elapsed_ms: 12000, deadline_ms: 60000 } } },
      { ...row(2), status: { type: "running", phase: { type: "creating_container" } } },
    ];
    const progress = deploymentProgressForEvent({ type: "progress", completed: 1, total: 3, rows }, rows, context);
    expect(JSON.stringify(progress)).not.toContain("never-publish");
    expect(JSON.stringify(progress)).not.toContain("environment");
    const view = deploymentView({ deployment: deployment("deploying"), progress, nodes: ["svc-0", "svc-1", "svc-2"].map((nodeId) => node({ nodeId, changed: true })) });
    expect(view.status).toBe("deploying");
    expect(view.nodes.map((n) => n.outcome)).toEqual(["deployed", "deploying", "deploying"]);
    expect(view.nodes[1]?.tail).toEqual(["server-1 · starting · 12s / 60s deadline"]);
    expect(view.nodes[2]?.tail).toEqual(["server-2 · Creating container"]);
  });

  it("times each node's rollout from when its rows were first seen started and finished", () => {
    const rows = [row(0), row(1)] as const;
    const running = deploymentProgressForEvent({ type: "progress", completed: 0, total: 2, rows: [{ ...rows[0], status: { type: "running", phase: { type: "starting" } } }, rows[1]] }, rows, { ...context, now: 1_000 });
    const progress = deploymentProgressForEvent({ type: "outcome", outcome: engineOrdered({ type: "success", completed: rows.map((r) => r.operation) } as const) }, rows, { ...context, prior: running, now: 13_000 });
    const view = deploymentView({ deployment: deployment("applied"), progress, nodes: [node({ nodeId: "svc-0", changed: true }), node({ nodeId: "svc-1", changed: true })] });
    expect(view.nodes.map((n) => [n.deploy, n.tail])).toEqual([
      [{ state: "done", durationMs: 12_000 }, ["server-0 · Starting replica · done"]],
      [{ state: "done", durationMs: 0 }, ["server-1 · Starting replica · done"]],
    ]);
  });

  it("matches the failed operation structurally in a mid-rollout failure, keeping its container ID", () => {
    const spec = resolvedServiceSpecFixture();
    const replacement = { machine_id: "machine-1" as MachineId, old_container_id: "old" as ContainerId, spec, skip_health_monitor: false };
    const rows = [row(0), row(1, { type: "replace_container", ...replacement }), row(2)] as const;
    const progress = deploymentProgressForEvent({ type: "outcome", outcome: engineOrdered({
      type: "failed", completed: [rows[0].operation], unexecuted: [rows[2].operation],
      failed: { type: "replacement_health", operation: replacement, error: { type: "health", container_id: "new-container" as ContainerId, failure: { type: "timed_out" } }, compensation: { type: "stop_first", stop_new_container: { type: "stopped" }, restart_old_container: { type: "failed", error: { type: "machine", action: "StartContainer", error: { code: "unavailable", message: "never-publish", details: { secret: "never-publish" } } } } } },
    } as const) }, rows, context);
    expect(progress.compensation).toEqual(["Replacement container stopped", "Previous container restart failed: StartContainer: unavailable"]);
    expect(JSON.stringify(progress)).not.toContain("never-publish");

    const view = deploymentView({ deployment: deployment("failed", { deployPreview: {} }), progress, nodes: [
      node({ nodeId: "svc-0", changed: true }), node({ nodeId: "svc-1", changed: true }), node({ nodeId: "svc-2", changed: true }), node({ nodeId: "db", changed: false }),
    ] });
    expect(view.nodes.map((n) => n.outcome)).toEqual(["deployed", "failed", "not_attempted", "unchanged"]);
    expect(view.nodes[1]).toMatchObject({ deploy: { state: "failed" }, failure: { message: "Health check timed out", containerId: "new-container" }, tail: ["server-1 · Health check timed out"] });
    expect(view.nodes[2]?.deploy.state).toBe("skipped");
    expect(view.nodes[3]).toMatchObject({ build: { state: "none" }, deploy: { state: "skipped" } });
    expect(deploymentStatusLabel(view)).toBe("Failed · 1 of 3 deployed");
  });

  it("blames the Image Build that failed, timing each image and tailing its error", () => {
    const view = deploymentView({
      deployment: deployment("failed", { failureMessage: "Image preparation failed" }),
      progress: { completed: 0, total: 0, outcome: null, rows: [], compensation: [], preparation: { phase: "build", serviceId: "web", machineId: "m", machineName: "builder", message: null } },
      nodes: [node({ nodeId: "api", changed: true, image: "api" }), node({ nodeId: "web", changed: true, image: "web" }),
        node({ nodeId: "docs", changed: true, image: "docs" }), node({ nodeId: "worker", changed: true })],
      buildLog: {
        steps: [
          step(1, 0, "stage:Upload", "Uploading source", 0, 1),
          step(2, 1, "stage:Building", "api", 1, 42), step(3, 1, "stage:Output", "Loading images", 42, 43),
          step(4, 2, "stage:Building", "web", 43, 50), step(5, 2, "sha256:a", "[2/3] RUN pnpm build", 44, 50, "exit code: 2"),
          step(6, 2, "stage:Cleanup", "Cleaning up", 50, 90),
        ],
        output: [{ stepId: 5, text: "\u001b[31msrc/a.ts(3,1): error TS2345: nope\u001b[0m\n" }, { stepId: 3, text: "loaded api\n" }],
      },
    });
    expect(view.nodes[0]).toMatchObject({ outcome: "not_attempted", build: { state: "done", durationMs: 42_000 }, deploy: { state: "skipped" }, tail: ["loaded api"] });
    expect(view.nodes[1]).toMatchObject({ outcome: "failed", build: { state: "failed", durationMs: 7_000 }, failure: { message: "Image preparation failed" },
      tail: ["src/a.ts(3,1): error TS2345: nope", "exit code: 2"] });
    expect(view.nodes[2]).toMatchObject({ outcome: "not_attempted", build: { state: "skipped" } });
    expect(view.nodes[3]).toMatchObject({ outcome: "not_attempted", build: { state: "none" }, deploy: { state: "skipped" } });
    expect(deploymentStatusLabel(view)).toBe("Failed · 0 of 4 deployed");
  });

  it("says which Server each image builds on and why, only once the Engine chose", () => {
    const view = deploymentView({
      deployment: deployment("queued"), progress: null,
      nodes: ["api", "web", "docs", "worker", "site", "blog", "wiki", "shop", "mail", "cron"].map((image) => node({ nodeId: image, changed: true, image })),
      buildLog: {
        steps: [], output: [],
        imageBuilds: [
          { image: "site", serverChoice: null, github: { runUrl: "https://github.com/o/r/actions/runs/1", reason: "first_in_build_order" }, skips: [] },
          { image: "api", serverChoice: { machineName: "nuc", reason: { kind: "had_cache" } }, github: null, skips: [] },
          { image: "web", serverChoice: { machineName: "hel-1", reason: { kind: "spread" } }, github: null, skips: [] },
          { image: "docs", serverChoice: { machineName: "hel-1", reason: { kind: "cache_holder_unavailable", holder: "c".repeat(32) as MachineId, name: "nuc" } }, github: null, skips: [] },
          { image: "worker", serverChoice: null, github: null, skips: [] },
          { image: "blog", serverChoice: null, github: { runUrl: "https://github.com/o/r/actions/runs/2", reason: "next_in_build_order" }, skips: [{ builder: "servers", kind: "not_started", minutes: 3 }] },
          { image: "wiki", serverChoice: null, github: null, skips: [{ builder: "github", kind: "no_workflow", repository: "o/r" }, { builder: "servers", kind: "preferred_unavailable", machineId: "e".repeat(32) as MachineId, name: null }] },
          { image: "shop", serverChoice: { machineName: "fast", reason: { kind: "preferred" } }, github: null, skips: [] },
          { image: "mail", serverChoice: null, github: { runUrl: "https://github.com/o/r/actions/runs/3", reason: "preferred" }, skips: [] },
          { image: "cron", serverChoice: { machineName: "hel-1", reason: { kind: "preferred_unavailable", preferred: "d".repeat(32) as MachineId, name: null } }, github: null, skips: [] },
        ],
      },
    });
    expect(view.nodes.map((n) => n.builtOn)).toEqual([
      { server: "nuc", reason: "had this Service's build cache", skipped: [] },
      { server: "hel-1", reason: "spread across Servers", skipped: [] },
      { server: "hel-1", reason: "nuc has the cache but is offline or no longer builds", skipped: [] },
      null,
      { server: "GitHub Actions", reason: "first in the build order", runUrl: "https://github.com/o/r/actions/runs/1", skipped: [] },
      { server: "GitHub Actions", reason: "next in the build order", runUrl: "https://github.com/o/r/actions/runs/2", skipped: ["Your servers: none started it in 3 min"] },
      { server: null, reason: null, skipped: ["GitHub: no workflow in o/r", "Preferred server: no longer in the Cluster"] },
      { server: "fast", reason: "preferred builder", skipped: [] },
      { server: "GitHub Actions", reason: "preferred builder", runUrl: "https://github.com/o/r/actions/runs/3", skipped: [] },
      { server: "hel-1", reason: "Preferred server: no longer in the Cluster", skipped: [] },
    ]);
    // The skip trail reads after the Builder that took the build, or alone before one did.
    expect(view.nodes.slice(5, 7).map((n) => n.builtOn && builtOnLine(n.builtOn))).toEqual([
      "Built on GitHub Actions · next in the build order · skipped Your servers: none started it in 3 min",
      "Skipped GitHub: no workflow in o/r · skipped Preferred server: no longer in the Cluster",
    ]);
  });

  it("tails the image building now while the next image waits its turn", () => {
    const view = deploymentView({
      deployment: deployment("planning"),
      progress: { completed: 0, total: 0, outcome: null, rows: [], compensation: [], preparation: { phase: "build", serviceId: "web", machineId: "m", machineName: "builder", message: null } },
      nodes: [node({ nodeId: "api", changed: true, image: "api" }), node({ nodeId: "web", changed: true, image: "web" })],
      buildLog: { steps: [step(1, 1, "stage:Building", "api", 0, null), step(2, 1, "sha256:a", "[1/2] RUN make", 1, null)],
        output: [{ stepId: 2, text: "one\ntwo\n" }, { stepId: 2, text: "three\n" }] },
    });
    expect(view.nodes.map((n) => [n.outcome, n.build.state, n.tail])).toEqual([["building", "running", ["two", "three"]], ["queued", "queued", []]]);
  });

  it("shows a queued attempt's Image Builds in parallel, and one failing while another still builds", () => {
    const view = deploymentView({
      deployment: deployment("queued"), progress: null,
      nodes: [node({ nodeId: "api", changed: true, image: "api" }), node({ nodeId: "web", changed: true, image: "web" }), node({ nodeId: "docs", changed: true, image: "docs" })],
      buildLog: {
        steps: [
          step(1, 1, "stage:Building", "api", 0, null, null, "api"), step(2, 1, "sha256:a", "[1/2] RUN make", 1, null, null, "api"),
          step(3, 1, "stage:Building", "web", 0, 5, null, "web"), step(4, 1, "sha256:b", "[1/2] RUN make", 1, 5, "exit code: 2", "web"),
          step(5, 0, "stage:Reused", "Reused image", 2, 2, null, "docs"),
        ],
        output: [{ stepId: 2, text: "compiling\n" }, { stepId: 4, text: "boom\n" }],
      },
    });
    expect(view.status).toBe("building");
    expect(view.nodes.map((n) => [n.outcome, n.build.state, n.tail])).toEqual([
      ["building", "running", ["compiling"]],
      ["failed", "failed", ["boom", "exit code: 2"]],
      ["queued", "done", []],
    ]);
    expect(view.nodes[1]?.failure).toEqual({ message: "Image build failed", containerId: null });
  });

  it("marks a node the attempt removed as Removed and counts it as deployed", () => {
    const remove = row(0, { type: "remove_container", machine_id: "machine-0" as MachineId, container_id: "gone" as ContainerId });
    const progress = deploymentProgressForEvent({ type: "outcome", outcome: engineOrdered({ type: "success", completed: [remove.operation] } as const) }, [remove], context);
    const view = deploymentView({ deployment: deployment("applied"), progress, nodes: [node({ nodeId: "svc-0", changed: true, removed: true }), node({ nodeId: "db", changed: false })] });
    expect(view.nodes.map((n) => n.outcome)).toEqual(["removed", "unchanged"]);
    expect(deploymentStatusLabel(view)).toBe("Deployed");
  });

  it("does not claim an unknown runtime outcome was never attempted", () => {
    const view = deploymentView({ deployment: deployment("failed", { failureMessage: "Connection lost", deployPreview: {} }), progress: null, nodes: [node({ nodeId: "web", changed: true })] });
    expect(view.nodes[0]).toMatchObject({ outcome: "failed", deploy: { state: "failed" }, failure: { message: "Connection lost" } });
  });
});
