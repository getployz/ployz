import { afterEach, describe, expect, it, vi } from "vitest";
import { hex } from "#/lib/bytes";
import { kept, view } from "#/modules/deploy-approval/rule-view.test-fixture";
import { answerRule, draftEnd, keptRule, RuleRefused, ruleOn } from "./auto-deploy-rule";

const sha256 = async (text: string) => hex(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(text)));

describe("draftEnd", () => {
  it("turns the rule off with its next generation, and refuses when none is on", () => {
    const ended = draftEnd(view(kept()), 5_000);
    expect(JSON.parse(ended.body)).toMatchObject({ generation: 2, ended: true, issued_at: 5_000, signer: "passkey" });
    expect(ruleOn(view({ body: ended.body, signatures: [] }))).toBe(false);
    expect(keptRule(view({ body: ended.body, signatures: [] }))?.ended).toBe(true);
    expect(() => draftEnd(view())).toThrow(RuleRefused);
  });
});

describe("answerRule", () => {
  afterEach(() => void vi.unstubAllGlobals());

  it("answers the rule's own challenge with a passkey prompt, carrying no signature", async () => {
    const bytes = (text: string) => new TextEncoder().encode(text).buffer;
    const get = vi.fn(async (_options: CredentialRequestOptions) => ({
      rawId: bytes("credential"),
      response: { authenticatorData: bytes("data"), clientDataJSON: bytes("{}"), signature: bytes("signature") },
    }));
    vi.stubGlobal("location", { hostname: "localhost" });
    vi.stubGlobal("navigator", { credentials: { get } });
    const draft = draftEnd(view(kept()), 5_000);
    const signed = await answerRule(draft);
    expect(signed).toEqual({
      body: draft.body,
      signatures: [],
      assertions: [{ credential_id: "Y3JlZGVudGlhbA", authenticator_data: "ZGF0YQ", client_data_json: "e30", signature: "c2lnbmF0dXJl" }],
    });
    const challenge = get.mock.calls[0]?.[0].publicKey?.challenge;
    expect(hex(challenge as Uint8Array)).toBe(await sha256(`ployz-authority/v1\nrule\n${draft.body}`));
    expect(get.mock.calls[0]?.[0].publicKey).toMatchObject({ rpId: "localhost", userVerification: "required" });
  });

  it("shows a passkey that didn't answer", async () => {
    vi.stubGlobal("location", { hostname: "localhost" });
    vi.stubGlobal("navigator", { credentials: { get: async () => null } });
    await expect(answerRule(draftEnd(view(kept())))).rejects.toThrow("Your passkey didn't answer. Try again.");
  });
});
