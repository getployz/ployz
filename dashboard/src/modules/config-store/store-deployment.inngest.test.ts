import { readFileSync } from "node:fs";
import { InngestTestEngine } from "@inngest/test";
import { Inngest } from "inngest";
import { describe, expect, it, vi } from "vitest";
import { createRunStoreDeployment } from "#/modules/config-store/store-deployment.inngest";
import { configDeploymentAdmittedEvent } from "#/modules/inngest/events";

const admitted = {
  name: configDeploymentAdmittedEvent,
  data: { organizationId: "org", environmentId: "env", deploymentId: "dep", trigger: "github" as const },
};

async function stepsOf(remove: boolean, status: "applied" | "failed" | "unknown" = "applied") {
  return (await runOf({ ran: { id: "dep", status, remove } })).order;
}

type Ran = { ran: { id: string; status: "applied" | "failed" | "unknown"; remove: boolean } } | { nothingToRun: string };

/** Runs the worker whose runner answers `ran`; resolves to its steps in order and the events it sent. */
async function runOf(ran: Ran) {
  const order: string[] = [];
  const results = new Map<string, unknown>([
    ["servers-lacking", []],
    ["plan-builds", []],
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
