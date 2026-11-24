import type { RuleTerms, RuleView, SavedRule } from "@ployz/sdk";
import { autoDeployRuleBody, autoDeployRuleEndBody } from "@ployz/sdk/authority";
import { describe, expect, it, vi } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { StoreRefused } from "#/lib/refusal";
import type { SaveTap } from "./deploy-tap.server";
import { type Answered, prepareSave, type Publish, type SaveAsk, type SaveSteps, saveWith, sendRule } from "./save-tap";

const here = { project: "shop", environment: "production" };
const CLUSTER = "CQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQk=";
const SEALED = "QUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQQ==";

const spec = (name: string) => ({
  name,
  mode: { mode: "replicated", replicas: 1 },
  container: {
    image: `ployz-build/${name}`,
    pull_policy: "never",
    environment: { SECRET: { sealed: SEALED, scope: "shared", key: "SECRET" } },
    sealed_identity: { organization: "org", environment: "11111111-1111-1111-1111-111111111111", service: "22222222-2222-2222-2222-222222222222" },
  },
});

/** The Store's staged rule view of a Save of web, which builds from main, at review `v1`. */
const view = (rule: RuleView["rule"] = null): RuleView =>
  // SAFETY: the fixture is the JSON the Store answers; ployz-core parses what it reads of it.
  JSON.parse(JSON.stringify({
    environment: { id: "env", project: "shop", name: "production", revision: 1 },
    namespace: "app",
    rule,
    sources: [{
      service: "web", repository: "acme/web", repository_id: 11, branch: "main", root_dir: ".",
      build: { buildMethod: "dockerfile", dockerfilePath: "Dockerfile" },
    }],
    target: [spec("web")],
    changed: [],
    version: "v1",
    protected: false,
  })) as RuleView;

const kept = () => ({
  body: autoDeployRuleBody(view(), asTestDouble<RuleTerms>()({
    cluster: CLUSTER, machines: ["1".repeat(32)], owner_id: 7, new_rule: "3".repeat(32), issued_at: 1_000, signer: "passkey",
  })).body,
  signatures: [],
});

const save: Publish = { command: "publish", environment: here, version: "v1", accept_volume_loss: [], message: null };

/** Cloud's reads of `staged`, and the gate it answers once asked. */
const reads = (staged: RuleView, gate: Extract<SaveTap, { kind: "view" }>["tap"]) =>
  vi.fn(async (asked: boolean): Promise<SaveTap> => ({ kind: "view", view: staged, tap: asked ? gate : null }));

const owner = async () => ({ ownerId: 7 });

describe("prepareSave", () => {
  it("saves with no Server reached when the person ended automatic deploys", async () => {
    const ended = view(kept());
    const read = reads(view({ body: autoDeployRuleEndBody(ended, { issued_at: 2_000, signer: "passkey" }).body, signatures: [] }), null);

    expect(await prepareSave(save, read, owner)).toEqual({ kind: "save", command: save });
    expect(read.mock.calls).toEqual([[false]]);
  });

  it("saves without the rule and says why when no passkey can be asked", async () => {
    const message = "Your servers don't trust your passkey. Add it under Account, Passkeys, or run ployz deploy.";
    const read = reads(view(), { kind: "refused", message });

    expect(await prepareSave(save, read, owner)).toEqual({ kind: "save", command: save, held: message });
  });

  it("saves without the rule and says why when GitHub can't name the owner", async () => {
    const read = reads(view(), asTestDouble<Extract<NonNullable<Extract<SaveTap, { kind: "view" }>["tap"]>, { kind: "ready" }>>()({
      kind: "ready", cluster: CLUSTER, passkeys: [], pins: {}, members: ["1".repeat(32)],
      against: { trusted: { devices: [], passkeys: [], grants: [] }, containers: [] },
    }));
    const unread = async () => ({ problem: "Couldn't read acme/web on GitHub." });

    expect(await prepareSave(save, read, unread)).toEqual({ kind: "save", command: save, held: "Couldn't read acme/web on GitHub." });
  });

  it("refuses only when the staged read fails", async () => {
    const read = async (): Promise<SaveTap> => ({ kind: "refused", message: "Couldn't read production." });

    expect(await prepareSave(save, read, owner)).toEqual({ kind: "refused", message: "Couldn't read production." });
  });
});

describe("saveWith", () => {
  it("sends a confirmed retry with the rule answered for its review, even once a fresh read needs none", async () => {
    const rule = asTestDouble<SavedRule>()({ prior: { rule: "9".repeat(32), generation: 3 } });
    const ask = asTestDouble<SaveAsk>()({ command: save, review: "v1" });
    const sent: Publish[] = [];
    const steps: SaveSteps = {
      answered: { current: null as Answered | null },
      commit: async (command) => {
        sent.push(command);
        if (sent.length === 1) throw new StoreRefused({ code: "confirmation_required", message: "Deletes data", details: null });
      },
      // The kept rule ended between the tap and the confirmation: a fresh read signs nothing.
      prepare: vi.fn().mockResolvedValueOnce({ kind: "ask", ask }).mockResolvedValue({ kind: "save", command: save }),
      ask: (asked, handles) => sendRule(steps, asked.command, { review: asked.review, rule }, handles),
      held: vi.fn(),
      refused: () => new Error("refused"),
    };

    await expect(saveWith(steps, save, ["confirmation_required"])).rejects.toBeInstanceOf(StoreRefused);
    const confirmed = { ...save, version: "v1#loss-k3x9", accept_volume_loss: ["data"] };
    await saveWith(steps, confirmed, ["confirmation_required"]);

    expect(sent[1]).toEqual({ ...confirmed, rule });
  });
});
