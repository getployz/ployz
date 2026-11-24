import type { PlanView, SavedRule } from "@ployz/sdk";
import { describe, expect, it, vi } from "vitest";
import { StoreRefused } from "#/lib/refusal";
import { asTestDouble } from "#/lib/test-double";
import { type AdmitDeploy, admitDeploy } from "#/modules/config-store/admit-deploy";
import { environmentKey } from "#/modules/config-store/environment-ref";
import { type Answer, type Ask, type DeploySteps, deployWith, prepareDeploy, sendAnswer } from "./deploy-tap";
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

  it("signs only the rule for a new Git Service that reaches data, and no version of it", async () => {
    const read = async () => ({
      kind: "plan" as const, plan: asTestDouble<PlanView>()({ version: "v1", namespace: "app", unsigned: [{ service: "web", needs: "rule" }], target: view().target }),
      tap: ready, source: { kind: "sign" as const, view: view(), unchecked: false },
    });

    expect(await prepareDeploy(admitDeploy(here, "d7"), read, async () => ({ ownerId: 7 }))).toMatchObject({
      kind: "ask", ask: { services: [], draft: { steps: [], source: { summary: { branch: "main" } } } },
    });
  });

  it("refuses a new Git Service before any prompt when the tap signs no rule over it", async () => {
    const read = async () => ({
      kind: "plan" as const, plan: asTestDouble<PlanView>()({ version: "v1", namespace: "app", unsigned: [{ service: "web", needs: "rule" }] }),
      tap: ready, source: { kind: "held" as const, reason: "An owner or admin approves this Deploy.", unchecked: false },
    });

    expect(await prepareDeploy(admitDeploy(here, "d9"), read, async () => ({ ownerId: 7 }))).toEqual({
      kind: "refused",
      message: "No automatic deploy rule covers web as it stands, so your servers won't build it. An owner or admin approves this Deploy.",
    });
  });

  it("deploys a Git Service its kept rule approves with no tap, and refuses one whose rule was ended", async () => {
    const read = (unsigned: PlanView["unsigned"]) => async () => ({
      kind: "plan" as const, plan: asTestDouble<PlanView>()({ version: "v1", namespace: "app", unsigned }), tap: ready,
    });
    const command = admitDeploy(here, "d8");

    expect(await prepareDeploy(command, read([{ service: "web", needs: "rebuild" }]))).toEqual({ kind: "admit", command: { ...command, version: "v1" }, held: null });
    expect(await prepareDeploy(command, read([{ service: "web", needs: "rule", ended: true }]))).toEqual({
      kind: "refused", message: "Automatic deploys are off for web. Turn on Deploy every push to deploy it.",
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

describe("deployWith", () => {
  it("sends a confirmed retry with the rule answered for its review, even once a fresh plan needs none", async () => {
    const answer: Answer = { versions: [], rule: asTestDouble<SavedRule>()({ prior: { rule: "9".repeat(32), generation: 3 } }) };
    const command = admitDeploy(here, "d6", { version: "v1" });
    const ask = asTestDouble<Ask>()({ command, plan: "v1", held: null });
    const sent: AdmitDeploy[] = [];
    const steps: DeploySteps = {
      answered: { current: null },
      commit: async (deploy) => {
        sent.push(deploy);
        if (sent.length === 1) throw new StoreRefused({ code: "confirmation_required", message: "Deletes data", details: null });
      },
      // The kept rule ended between the tap and the confirmation: a fresh plan signs nothing.
      prepare: vi.fn().mockResolvedValueOnce({ kind: "ask", ask }).mockResolvedValue({ kind: "admit", command, held: null }),
      ask: (asked, handles) => sendAnswer(steps, asked.command, { plan: asked.plan, place: environmentKey(here), answer, held: null }, handles),
      stale: async () => new Error("stale"),
      refused: () => new Error("refused"),
      held: vi.fn(),
    };

    await expect(deployWith(steps, command, ["confirmation_required"])).rejects.toBeInstanceOf(StoreRefused);
    const confirmed = admitDeploy(here, "d6", { version: "v1#loss-k3x9", acceptVolumeLoss: ["pg-data"] });
    await deployWith(steps, confirmed, ["confirmation_required"]);

    expect(sent[1]).toEqual({ ...confirmed, versions: [], rule: answer.rule });
  });
});
