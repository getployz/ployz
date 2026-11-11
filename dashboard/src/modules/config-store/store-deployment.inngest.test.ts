import { readFileSync } from "node:fs";
import { InngestTestEngine } from "@inngest/test";
import { Inngest } from "inngest";
import { describe, expect, it, vi } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { APPROVAL_LOOKS, createRunStoreDeployment, walkApproval } from "#/modules/config-store/store-deployment.inngest";
import { configDeploymentAdmittedEvent, configDeploymentApprovedEvent } from "#/modules/inngest/events";

const admitted = {
  name: configDeploymentAdmittedEvent,
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

describe("a redeploy that waits for its commits to be approved again", () => {
  async function walk(awaiting: (look: number) => boolean) {
    const order: string[] = [];
    const step = {
      run: async (name: string) => { order.push(name); return awaiting(Number(name.slice("approval-".length))); },
      waitForEvent: async (name: string, options: { event: string; if: string }) => {
        order.push(name);
        expect(options).toMatchObject({ event: configDeploymentApprovedEvent, if: "async.data.deploymentId == 'dep'" });
        return null;
      },
    };
    const approved = await walkApproval(
      admitted.data,
      asTestDouble<Parameters<typeof walkApproval>[1]>()(step),
      asTestDouble<Parameters<typeof walkApproval>[2]>()(() => undefined),
    );
    return { approved, order };
  }

  it("runs once the approval lands, looking again after each wait", async () => {
    const { approved, order } = await walk((look) => look < 2);
    expect(approved).toBe(true);
    expect(order).toEqual(["approval-0", "approval-wait-0", "approval-1", "approval-wait-1", "approval-2"]);
  });

  it("runs an approval that lands during the last wait", async () => {
    const { approved, order } = await walk((look) => look < APPROVAL_LOOKS);
    expect(approved).toBe(true);
    expect(order.slice(-2)).toEqual([`approval-wait-${APPROVAL_LOOKS - 1}`, `approval-${APPROVAL_LOOKS}`]);
  });

  it("gives up once no approval came within the hour", async () => {
    const { approved, order } = await walk(() => true);
    expect(approved).toBe(false);
    expect(order.filter((name) => name.startsWith("approval-wait-"))).toHaveLength(APPROVAL_LOOKS);
    expect(order.at(-1)).toBe(`approval-${APPROVAL_LOOKS}`);
  });
});

describe("a Deploy's storage steps", () => {
  it("never sleep or wait on a run the Deploy did not start", () => {
    const deployment = read("./store-deployment.inngest.ts");
    expect(deployment).not.toContain("placement-wait");
    expect(deployment).not.toContain("step.sleep");
    expect(read("../volume-placement/volume-placement.deploy.server.ts")).not.toContain("listActiveVolumeRuns");
  });
});
