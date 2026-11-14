import type { RuleView } from "@ployz/sdk";
import { afterEach, describe, expect, it, vi } from "vitest";
import { hex } from "#/modules/approval-session/approval-session";
import { answerRule, draftedRepository, draftEnd, draftRule, keptRule, RuleRefused, ruleOn } from "./auto-deploy-rule";

/** `rule_draft_tests::GOLDEN`: the CLI and the Servers draft these bytes from the same view and terms. */
const GOLDEN = "5fe1b138faa40462100905f967e762cae20eec664bf4a2c9e8e40066ec9fcd20";
const SEALED = "QUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQQ==";
const BUILDER_SHA = "39e1f488e492c0641e520777e481e8f5b266985b";

const spec = (name: string, plain: string) => ({
  name,
  mode: { mode: "replicated", replicas: 1 },
  container: {
    image: `ployz-build/${name}`,
    pull_policy: "never",
    environment: { PLAIN: plain, SECRET: { sealed: SEALED, scope: "shared", key: "SECRET" } },
    sealed_identity: {
      organization: "org",
      environment: "11111111-1111-1111-1111-111111111111",
      service: "22222222-2222-2222-2222-222222222222",
    },
  },
  pre_deploy: { command: ["migrate"] },
});

/** `rule_draft_tests::view_json`, as the Store's rule view writes it. */
const view = (rule: RuleView["rule"] = null, web = "one"): RuleView =>
  // SAFETY: the fixture is the JSON the Store answers; `autoDeployRuleBody` parses what it reads of it.
  JSON.parse(JSON.stringify({
    environment: { id: "ignored" },
    namespace: "app",
    rule,
    sources: [{
      service: "web",
      repository: "Ployz-test/lab-service",
      repository_id: 1_450_985_696,
      branch: "main",
      root_dir: ".",
      build: { buildMethod: "dockerfile", dockerfilePath: "Dockerfile" },
    }],
    target: [spec("web", web), spec("cache", "two")],
    changed: [],
  })) as RuleView;

const scope = {
  environmentId: "11111111-1111-1111-1111-111111111111",
  environmentName: "production",
  namespace: "app",
  cluster: "CQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQk=",
  machines: ["2".repeat(32), "1".repeat(32), "2".repeat(32)],
};

const sha256 = async (text: string) => hex(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(text)));

const newRule = () => vi.spyOn(crypto, "getRandomValues").mockImplementation((array) => {
  if (array instanceof Uint8Array) array.fill(0x33);
  return array;
});

describe("draftRule", () => {
  afterEach(() => void vi.restoreAllMocks());

  it("drafts the bytes the CLI drafts, with Ployz's pinned builder, for a passkey's own prompt", async () => {
    newRule();
    const draft = draftRule(view(), scope, 278_111_222, 1_000);
    expect(await sha256(draft.body)).toBe(GOLDEN);
    expect(JSON.parse(draft.body)).toMatchObject({ signer: "passkey", generation: 1, source: { builder: { sha: BUILDER_SHA } } });
    expect(draft.summary).toMatchObject({ repository: "Ployz-test/lab-service", branch: "main", pinned: true, servers: 2 });
    expect(draftedRepository(draft)).toBe("Ployz-test/lab-service");
    expect(draft.summary.services.map((service) => service.name)).toEqual(["web"]);
  });

  it("approves the kept rule's next generation and names what changed", () => {
    newRule();
    const first = draftRule(view(), scope, 278_111_222, 1_000);
    const kept = view({ body: first.body, signatures: [] });
    const changed = view({ body: first.body, signatures: [] }, "changed");
    expect(ruleOn(kept)).toBe(true);
    const next = draftRule(changed, scope, 278_111_222, 2_000);
    expect(JSON.parse(next.body)).toMatchObject({ rule: "33".repeat(16), generation: 2 });
    expect(next.summary.services[0]?.changes).toEqual(["settings", "build args"]);
  });

  it("turns the rule off with its next generation, and refuses when none is on", () => {
    newRule();
    const first = draftRule(view(), scope, 278_111_222, 1_000);
    const ended = draftEnd(view({ body: first.body, signatures: [] }), 5_000);
    expect(JSON.parse(ended.body)).toMatchObject({ generation: 2, ended: true, issued_at: 5_000, signer: "passkey" });
    expect(ruleOn(view({ body: ended.body, signatures: [] }))).toBe(false);
    expect(keptRule(view({ body: ended.body, signatures: [] }))?.ended).toBe(true);
    expect(() => draftEnd(view())).toThrow(RuleRefused);
  });

  it("starts a new rule on the branch the Services build from now, even after turning the old one off", () => {
    newRule();
    const first = draftRule(view(), scope, 278_111_222, 1_000);
    const ended = draftEnd(view({ body: first.body, signatures: [] }), 2_000);
    for (const rule of [first, ended]) {
      const moved = view({ body: rule.body, signatures: [] });
      for (const source of moved.sources) source.branch = "release";
      const next = draftRule(moved, scope, 278_111_222, 3_000);
      expect(JSON.parse(next.body)).toMatchObject({ generation: 1, source: { branch: "release" } });
    }
  });

  it("says why a Service can't be in a rule", () => {
    const twoBranches = view();
    const [web] = twoBranches.sources;
    if (web === undefined) expect.fail("no source");
    twoBranches.sources.push({ ...web, service: "cache", branch: "release" });
    expect(() => draftRule(twoBranches, scope, 1)).toThrow(/A rule deploys one branch/);
  });
});

describe("answerRule", () => {
  afterEach(() => void vi.unstubAllGlobals());

  it("answers the rule's own challenge with a passkey prompt, carrying no signature and no grant", async () => {
    const bytes = (text: string) => new TextEncoder().encode(text).buffer;
    const get = vi.fn(async (_options: CredentialRequestOptions) => ({
      rawId: bytes("credential"),
      response: { authenticatorData: bytes("data"), clientDataJSON: bytes("{}"), signature: bytes("signature") },
    }));
    vi.stubGlobal("location", { hostname: "localhost" });
    vi.stubGlobal("navigator", { credentials: { get } });
    const draft = draftRule(view(), scope, 278_111_222, 1_000);
    const signed = await answerRule(draft);
    expect(signed).toEqual({
      body: draft.body,
      signatures: [],
      assertions: [{ credential_id: "Y3JlZGVudGlhbA", authenticator_data: "ZGF0YQ", client_data_json: "e30", signature: "c2lnbmF0dXJl" }],
    });
    expect(signed.grants).toBeUndefined();
    const challenge = get.mock.calls[0]?.[0].publicKey?.challenge;
    expect(hex(challenge as Uint8Array)).toBe(await sha256(`ployz-authority/v1\nrule\n${draft.body}`));
    expect(get.mock.calls[0]?.[0].publicKey).toMatchObject({ rpId: "localhost", userVerification: "required" });
  });

  it("shows a passkey that didn't answer", async () => {
    vi.stubGlobal("location", { hostname: "localhost" });
    vi.stubGlobal("navigator", { credentials: { get: async () => null } });
    await expect(answerRule(draftRule(view(), scope, 1))).rejects.toThrow("Your passkey didn't answer. Try again.");
  });
});
