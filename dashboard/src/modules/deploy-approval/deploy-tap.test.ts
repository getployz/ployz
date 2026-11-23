import type { PlanView } from "@ployz/sdk";
import { describe, expect, it, vi } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { admitDeploy } from "#/modules/config-store/admit-deploy";
import { prepareDeploy } from "./deploy-tap";

const here = { project: "shop", environment: "production" };
const plan = (version: string) => async () => asTestDouble<PlanView>()({ version, namespace: "shop-production", unsigned: [{ service: "postgres", needs: "version" }] });

describe("prepareDeploy", () => {
  it("signs the review a Volume-loss confirm bound, digest and loss mark included", async () => {
    const servers = vi.fn(async () => ({ kind: "off" as const }));
    const command = admitDeploy(here, "d1", { version: "7:3:2.0:ab12#loss-k3x9", acceptVolumeLoss: ["pg-data"] });
    expect(await prepareDeploy(command, plan("7:3:2.0"), servers)).toEqual({ kind: "admit", command });
  });

  it("asks nothing when the plan moved past the review the person read", async () => {
    const servers = vi.fn(async () => ({ kind: "off" as const }));
    expect(await prepareDeploy(admitDeploy(here, "d2", { version: "7:3:2.0" }), plan("8:3:2.0"), servers)).toEqual({ kind: "stale" });
    expect(servers).not.toHaveBeenCalled();
  });
});
