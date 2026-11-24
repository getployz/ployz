import type { PlanView } from "@ployz/sdk";
import { describe, expect, it } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { admitDeploy } from "#/modules/config-store/admit-deploy";
import { prepareDeploy } from "./deploy-tap";
import { ready, view } from "./rule-view.test-fixture";

const here = { project: "shop", environment: "production" };
const plan = (version: string) => async () => ({
  kind: "plan" as const,
  plan: asTestDouble<PlanView>()({ version, namespace: "shop-production", unsigned: [{ service: "postgres", needs: "version" }] }),
  tap: { kind: "off" as const },
});

describe("prepareDeploy", () => {
  it("admits a review bound to a loss and a digest", async () => {
    const command = admitDeploy(here, "d1", { version: "7:3:2.0:ab12#loss-k3x9", acceptVolumeLoss: ["pg-data"] });
    expect(await prepareDeploy(command, plan("7:3:2.0"))).toEqual({ kind: "admit", command, held: null });
  });

  it("admits a Deploy that names no version at the plan it read", async () => {
    const command = admitDeploy(here, "d2");
    expect(await prepareDeploy(command, plan("7:3:2.0"))).toEqual({ kind: "admit", command: { ...command, version: "7:3:2.0" }, held: null });
  });

  it("asks nothing when the plan moved past the review the person read", async () => {
    expect(await prepareDeploy(admitDeploy(here, "d3", { version: "7:3:2.0" }), plan("8:3:2.0"))).toEqual({ kind: "stale" });
  });

  it("signs the automatic deploy rule in the Deploy's own tap when no version needs one", async () => {
    const read = async () => ({ kind: "plan" as const, plan: asTestDouble<PlanView>()({ version: "v1", namespace: "app", unsigned: [] }), tap: ready, source: { kind: "sign" as const, view: view(), unchecked: false } });

    expect(await prepareDeploy(admitDeploy(here, "d4"), read, async () => ({ ownerId: 7 }))).toMatchObject({
      kind: "ask", ask: { services: [], prior: null, draft: { source: { summary: { branch: "main" } } }, command: { version: "v1" } },
    });
  });

  it("deploys without the rule and says why when GitHub can't name its owner", async () => {
    const read = async () => ({ kind: "plan" as const, plan: asTestDouble<PlanView>()({ version: "v1", namespace: "app", unsigned: [] }), tap: ready, source: { kind: "sign" as const, view: view(), unchecked: false } });
    const command = admitDeploy(here, "d5");

    expect(await prepareDeploy(command, read, async () => ({ problem: "Couldn't read acme/web on GitHub." }))).toEqual({
      kind: "admit", command: { ...command, version: "v1" }, held: { reason: "Couldn't read acme/web on GitHub.", unchecked: false },
    });
  });
});
