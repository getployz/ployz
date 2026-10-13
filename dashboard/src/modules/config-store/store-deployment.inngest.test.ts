import { InngestTestEngine } from "@inngest/test";
import { Inngest } from "inngest";
import { describe, expect, it } from "vitest";
import { createRunStoreDeployment } from "#/modules/config-store/store-deployment.inngest";
import { configDeploymentAdmittedEvent } from "#/modules/inngest/events";

const admitted = {
  name: configDeploymentAdmittedEvent,
  data: { organizationId: "org", environmentId: "env", deploymentId: "dep" },
};

async function stepsOf(remove: boolean) {
  const order: string[] = [];
  const results = new Map<string, unknown>([
    ["servers-lacking", []],
    ["plan-builds", []],
    ["placement-0", { kind: "stopped" }],
    ["run-deployment", { ran: { id: "dep", status: "applied", remove } }],
    ["sweep", { deployments: [], closing: [] }],
    ["delete-orphan-mirrors", []],
  ]);
  const ids = ["record-run", ...results.keys(), "sync-uptime", "forget-run"];
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
