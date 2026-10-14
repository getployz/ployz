import { readFileSync } from "node:fs";
import { InngestTestEngine } from "@inngest/test";
import { Inngest } from "inngest";
import { describe, expect, it } from "vitest";
import { createRunStoreDeployment } from "#/modules/config-store/store-deployment.inngest";
import { configDeploymentAdmittedEvent } from "#/modules/inngest/events";

const admitted = {
  name: configDeploymentAdmittedEvent,
  data: { organizationId: "org", environmentId: "env", deploymentId: "dep", trigger: "github" as const },
};

async function stepsOf(remove: boolean, status: "applied" | "failed" | "unknown" = "applied") {
  const order: string[] = [];
  const results = new Map<string, unknown>([
    ["servers-lacking", []],
    ["plan-builds", []],
    ["placement-0", { kind: "stopped" }],
    ["place-volumes", []],
    ["run-deployment", { ran: { id: "dep", status, remove } }],
    ["finish-volumes", null],
    ["sweep", { deployments: [], closing: [] }],
    ["delete-orphan-mirrors", []],
  ]);
  const ids = ["record-run", ...results.keys(), "mail-failure", "sync-uptime", "forget-run"];
  await new InngestTestEngine({
    function: createRunStoreDeployment(new Inngest({ id: "store-deployment-order-test" })),
    events: [admitted],
    steps: ids.map((id) => ({ id, handler: () => { order.push(id); return results.get(id) ?? null; } })),
  }).execute();
  return order;
}

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
  it("is sent once it ended failed or unknown, before the checks follow it", async () => {
    for (const status of ["failed", "unknown"] as const) {
      const order = await stepsOf(false, status);
      expect(order.slice(order.indexOf("run-deployment"), order.indexOf("sync-uptime")))
        .toEqual(["run-deployment", "finish-volumes", "mail-failure"]);
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
