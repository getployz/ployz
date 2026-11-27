import { readFileSync } from "node:fs";
import { InngestTestEngine } from "@inngest/test";
import { Inngest } from "inngest";
import { describe, expect, it, vi } from "vitest";
import { createRunStoreDeployment } from "#/modules/config-store/store-deployment.inngest";
import { configDeploymentAdmitted } from "#/modules/config-store/config-store.events";

const admitted = {
  name: configDeploymentAdmitted.name,
  data: { organizationId: "org", environmentId: "env", deploymentId: "dep", trigger: "github" as const },
};

async function stepsOf(remove: boolean, status: "applied" | "failed" | "unknown" = "applied") {
  return (await runOf({ ran: { id: "dep", status, remove } })).order;
}

type Ran = { ran: { id: string; status: "applied" | "failed" | "unknown"; remove: boolean } } | { nothingToRun: string };

async function runOf(ran: Ran) {
  const order: string[] = [];
  const results = new Map<string, unknown>([
    ["servers-lacking", []],
    ["plan-builds", []],
    ["approval-0", false],
    ["placement-0", { kind: "stopped" }],
    ["place-volumes", []],
    ["run-deployment", ran],
    ["finish-volumes", null],
    ["sweep", { deployments: [], closing: [] }],
    ["delete-orphan-mirrors", []],
  ]);
  const ids = ["record-run", ...results.keys(), "retire-old", "mail-failure", "sync-uptime", "forget-run"];
  const { ctx } = await new InngestTestEngine({
    function: createRunStoreDeployment(new Inngest({ id: "store-deployment-order-test" })),
    events: [admitted],
    steps: ids.map((id) => ({ id, handler: () => { order.push(id); return results.get(id) ?? null; } })),
  }).execute();
  return { order, sent: vi.mocked(ctx.step.sendEvent).mock.calls };
}

describe("a Deployment's cleanup", () => {
  it("is asked for once its runner ran it, whatever the outcome", async () => {
    for (const status of ["applied", "failed", "unknown"] as const) {
      const { sent } = await runOf({ ran: { id: "dep", status, remove: false } });
      expect(sent).toEqual([["retire-old", {
        id: "config-deployment-retire-requested-dep", name: "config/deployment.retire-requested",
        data: { organizationId: "org", environmentId: "env", deploymentId: "dep" },
      }]]);
    }
  });

  it("is not asked for when nothing ran", async () => {
    expect((await runOf({ nothingToRun: "Another runner owns it." })).sent).toEqual([]);
  });
});

describe("an applied Deployment's orphan mirror scan", () => {
  it("runs after a removal's sweep, once the removed Environment no longer holds its Volumes' names", async () => {
    const order = await stepsOf(true);
    expect(order.slice(-2)).toEqual(["sweep", "delete-orphan-mirrors"]);
  });

  it("runs without a sweep for a Deployment that removes nothing", async () => {
    const order = await stepsOf(false);
    expect(order).not.toContain("sweep");
    expect(order.at(-1)).toBe("delete-orphan-mirrors");
  });
});

describe("a Deployment's failure email", () => {
  it("is sent once it ended failed or unknown, after the run is wrapped up", async () => {
    for (const status of ["failed", "unknown"] as const) {
      const order = await stepsOf(false, status);
      expect(order.slice(order.indexOf("run-deployment")))
        .toEqual(["run-deployment", "finish-volumes", "retire-old", "sync-uptime", "forget-run", "mail-failure"]);
    }
  });

  it("is never sent for one that applied", async () => {
    expect(await stepsOf(false)).not.toContain("mail-failure");
  });
});

const read = (path: string) => readFileSync(new URL(path, import.meta.url), "utf8");

describe("a Deploy's storage steps", () => {
  it("never sleep or wait on a run the Deploy did not start", () => {
    const deployment = read("./store-deployment.inngest.ts");
    expect(deployment).not.toContain("placement-wait");
    expect(deployment).not.toContain("step.sleep");
    expect(read("../volume-placement/volume-placement.deploy.server.ts")).not.toContain("listActiveVolumeRuns");
  });
});

describe("a Deployment with several Git Services", () => {
  const target = (service: string) => ({ build: `dep.${service}`, service, repository: "acme/web", repositoryId: 42, installationId: 7, hasNext: false });
  const builds = [target("web"), target("api"), target("docs")];
  const fresh = () => { throw new Error("a step ran its real effect"); };

  async function stepsRun(memo: ReadonlyArray<readonly [string, unknown]>) {
    const results = new Map<string, unknown>([
      ["servers-lacking", []],
      ["plan-builds", builds],
      ["placement-0", { kind: "stopped" }],
      ["place-volumes", []],
      ["run-deployment", { ran: { id: "dep", status: "applied", remove: false } }],
      ["finish-volumes", null],
      ["delete-orphan-mirrors", []],
      ...memo,
    ]);
    const ran: string[] = [];
    await new InngestTestEngine({
      function: createRunStoreDeployment(new Inngest({ id: "store-deployment-builds-test" }), fresh),
      events: [admitted],
      steps: [...results.keys(), "record-run", "github-failed", "retire-old", "sync-uptime", "forget-run"]
        .map((id) => ({ id, handler: () => { ran.push(id); return results.get(id) ?? null; } })),
    }).execute();
    return ran;
  }

  it("starts every build in one dispatch step and one record step, and runs the Deployment once", async () => {
    const ran = await stepsRun([
      ["github-dispatch", [{ kind: "run", build: "dep.web", run: {} }, { kind: "run", build: "dep.api", run: {} }, { kind: "skip", build: "dep.docs", message: "no workflow" }]],
      ["github-record", [{ kind: "dispatched", runId: 9001 }, { kind: "dispatched", runId: 9002 }, { kind: "done" }]],
      ["github-open-web-0", false],
      ["github-open-api-0", false],
    ]);
    expect(ran.filter((id) => id.startsWith("github-"))).toEqual(["github-dispatch", "github-record", "github-open-web-0", "github-open-api-0"]);
    expect(ran.filter((id) => id === "run-deployment")).toHaveLength(1);
    expect(ran).not.toContain("github-failed");
  });

  it("runs a run that started its builds under older step ids to the end, through the dispatch and record steps", async () => {
    // What the older per-build steps recorded; the dispatch step follows the dispatched runs and leaves the skipped build.
    const ran = await stepsRun([
      ["github-start-web", { kind: "dispatched", runId: 9001 }],
      ["github-start-api", { kind: "dispatched", runId: 9002 }],
      ["github-start-docs", { kind: "done" }],
      ["github-dispatch", [{ kind: "dispatched", runId: 9001 }, { kind: "dispatched", runId: 9002 }, { kind: "done" }]],
      ["github-record", [{ kind: "dispatched", runId: 9001 }, { kind: "dispatched", runId: 9002 }, { kind: "done" }]],
      ["github-open-web-0", false],
      ["github-open-api-0", false],
    ]);
    expect(ran.filter((id) => id.startsWith("github-start-"))).toEqual([]);
    expect(ran.filter((id) => id === "github-dispatch" || id === "github-record")).toEqual(["github-dispatch", "github-record"]);
    expect(ran.filter((id) => id === "run-deployment")).toHaveLength(1);
  });
});
