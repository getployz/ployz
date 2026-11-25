import type { PlanView, RuleView, SavedRule } from "@ployz/sdk";
import { describe, expect, it, vi } from "vitest";
import { StoreRefused } from "#/lib/refusal";
import { asTestDouble } from "#/lib/test-double";
import { type AdmitDeploy, admitDeploy } from "#/modules/config-store/admit-deploy";
import { environmentKey } from "#/modules/config-store/environment-ref";
import { type Answer, type Ask, type DeploySteps, deployWith, prepareDeploy, sendAnswer } from "./deploy-tap";
import type { DeployPlan, DeploySource } from "./deploy-tap.server";
import { kept, ready, view } from "./rule-view.test-fixture";

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
      tap: ready, source: { kind: "sign" as const, view: view(), unchecked: false }, rules: view(),
    });

    expect(await prepareDeploy(admitDeploy(here, "d7"), read, async () => ({ ownerId: 7 }))).toMatchObject({
      kind: "ask", ask: { services: [], draft: { steps: [], source: { summary: { branch: "main" } } } },
    });
  });

  /** A Deploy of web from source on Servers that enforce, as Cloud read it: `rules` is the staged rule view, null unread. */
  const building = (rules: RuleView | null, source?: DeploySource) => async (): Promise<DeployPlan> => {
    const planned = { kind: "plan" as const, plan: asTestDouble<PlanView>()({ version: "v1", namespace: "app", unsigned: [], target: view().target }), tap: ready, rules };
    return source === undefined ? planned : { ...planned, source };
  };

  it("refuses a Git Service before any prompt when the tap signs no rule over it", async () => {
    const held = { kind: "held" as const, reason: "Upgrade web-1 first. Its ployz can't check your passkey.", unchecked: false };

    expect(await prepareDeploy(admitDeploy(here, "d9"), building(view(), held), async () => ({ ownerId: 7 }))).toEqual({
      kind: "refused",
      message: "No automatic deploy rule covers web as it stands, so your servers won't build it. Upgrade web-1 first. Its ployz can't check your passkey.",
    });
  });

  it("refuses every Git Service when Cloud couldn't read the rule", async () => {
    const unread = { kind: "held" as const, reason: "Couldn't check automatic deploys.", unchecked: true };

    expect(await prepareDeploy(admitDeploy(here, "d10"), building(null, unread), async () => ({ ownerId: 7 }))).toEqual({
      kind: "refused", message: "Couldn't read the automatic deploy rule, so web can't build under it.",
    });
  });

  it("checks a Git Service the plan doesn't ask a version of against the kept rule's settings", async () => {
    // web reaches no data, so the plan lists it nowhere, and its settings moved past what the kept rule approves.
    const moved = { ...view(kept()), changed: ["web"] };

    expect(await prepareDeploy(admitDeploy(here, "d11"), building(moved))).toEqual({
      kind: "refused", message: "No automatic deploy rule covers web as it stands, so your servers won't build it. Turn on Deploy every push, then deploy again.",
    });
  });

  it("deploys a Git Service its kept rule approves with no tap, and refuses one whose rule was ended", async () => {
    const command = admitDeploy(here, "d8");
    const body = kept();
    const ended = { ...body, body: JSON.stringify({ ...JSON.parse(body.body), ended: true }) };

    expect(await prepareDeploy(command, building(view(body)))).toEqual({ kind: "admit", command: { ...command, version: "v1" }, held: null });
    expect(await prepareDeploy(command, building(view(ended)))).toEqual({
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
