import { afterEach, describe, expect, it, vi } from "vitest";
import type { Review } from "#/modules/approval-session/approval-prompt";
import * as passkey from "#/modules/approval-session/passkey-statement";
import type { ActivationMember, PasskeyAnswered, RecipientInstall, ServerActivation } from "./activation";
import { ActivationRefused, type ActivationSteps, runActivation } from "./activation-flow";

const IDENTITY = "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=";
const RECIPIENT = "AgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgI=";
const WEB_1 = "1".repeat(32);
const WEB_2 = "2".repeat(32);
const ASSERTION = { credential_id: "AQID", authenticator_data: "AQID", client_data_json: "AQID", signature: "AQID" };
const member = (id: string, fill: number): ActivationMember => ({ id, public_key: Array.from({ length: 32 }, () => fill), subnet: `10.210.${fill}.0/24` });
const OFFER = { cluster: IDENTITY, members: [member(WEB_2, 2), member(WEB_1, 1)] };
const NAMES = { [WEB_1]: "web-1", [WEB_2]: "web-2" };

const activated = (id: string, name: string): ServerActivation => ({ id, name, kind: "activated", privateAccess: null });
const installed = (id: string, name: string): RecipientInstall => ({ id, name, kind: "installed" });

/** Each step as Cloud would answer it, with every call kept. */
function steps(answers: { review?: boolean[]; servers?: readonly ServerActivation[]; generate?: () => Promise<{ generator: string; recipient: string }> } = {}) {
  const reviews: Review[] = [];
  const answersLeft = [...(answers.review ?? [true, true])];
  const called = {
    reviews,
    steps: [] as string[],
    activate: vi.fn(async (_set: PasskeyAnswered) => answers.servers ?? [activated(WEB_1, "web-1"), activated(WEB_2, "web-2")]),
    generate: vi.fn(answers.generate ?? (async (_operation: string) => ({ generator: WEB_1, recipient: RECIPIENT }))),
    install: vi.fn(async (_rotation: PasskeyAnswered, _generator: string) => [installed(WEB_1, "web-1"), installed(WEB_2, "web-2")]),
  };
  const run: ActivationSteps = {
    review: async (review) => {
      reviews.push(review);
      return answersLeft.shift() ?? false;
    },
    activate: called.activate,
    generate: called.generate,
    install: called.install,
    step: (step) => called.steps.push(step),
  };
  return { called, run };
}

describe("runActivation", () => {
  afterEach(() => vi.restoreAllMocks());
  const answering = () => vi.spyOn(passkey, "signWithPasskey")
    .mockImplementation(async (body) => ({ body, signatures: [], assertions: [ASSERTION] }));

  it("asks two passkey prompts, each over the body it shows, and installs the key the Server made", async () => {
    const sign = answering();
    const { called, run } = steps();

    const outcome = await runActivation(OFFER, NAMES, run);

    const [setBody, setPurpose] = sign.mock.calls[0] ?? [];
    const [rotationBody, rotationPurpose] = sign.mock.calls[1] ?? [];
    expect([setPurpose, rotationPurpose]).toEqual(["enforce", "enforce"]);
    const set = JSON.parse(setBody ?? "");
    expect(set.members.map((one: ActivationMember) => one.id)).toEqual([WEB_1, WEB_2]);
    expect(set.signer).toBe("passkey");
    expect(set).not.toHaveProperty("kept_keys");
    expect(called.reviews[0]?.lines[0]).toBe("Approved: turn on device enforcement for good on these servers. There's no off.");
    expect(called.reviews[0]?.lines[1]).toMatch(/^ {2}web-1, key .+, subnet 10\.210\.1\.0\/24, ID 1{32}$/u);
    expect(called.activate).toHaveBeenCalledWith({ body: setBody, signatures: [], assertions: [ASSERTION] });
    expect(called.generate).toHaveBeenCalledWith(set.operation);

    const rotation = JSON.parse(rotationBody ?? "");
    expect(rotation).toMatchObject({ cluster: IDENTITY, operation: set.operation, recipient: RECIPIENT, members: set.members, signer: "passkey" });
    expect(called.reviews[1]?.lines[0]).toBe(`Approved: seal new values to key "${RECIPIENT}". Only these servers get it:`);
    expect(called.install).toHaveBeenCalledWith({ body: rotationBody, signatures: [], assertions: [ASSERTION] }, WEB_1);
    expect(called.steps).toEqual(["approving", "activating", "generating", "approving", "installing"]);
    expect(outcome).toEqual({
      servers: [activated(WEB_1, "web-1"), activated(WEB_2, "web-2")],
      key: { kind: "installed", installs: [installed(WEB_1, "web-1"), installed(WEB_2, "web-2")] },
    });
  });

  it("activates nothing when the person cancels the first prompt", async () => {
    const sign = answering();
    const { called, run } = steps({ review: [false] });

    await expect(runActivation(OFFER, NAMES, run)).rejects.toEqual(new ActivationRefused("You cancelled. Nothing was activated."));
    expect(sign).not.toHaveBeenCalled();
    expect(called.activate).not.toHaveBeenCalled();
  });

  it("activates nothing when the passkey doesn't answer", async () => {
    vi.spyOn(passkey, "signWithPasskey").mockRejectedValue(new passkey.PasskeyRefused("Your passkey didn't answer. Try again."));
    const { called, run } = steps();

    await expect(runActivation(OFFER, NAMES, run)).rejects.toEqual(new ActivationRefused("Your passkey didn't answer. Try again. Nothing was activated."));
    expect(called.activate).not.toHaveBeenCalled();
  });

  it("keeps the activation but sets no key when the person cancels the second prompt", async () => {
    answering();
    const { called, run } = steps({ review: [true, false] });

    const outcome = await runActivation(OFFER, NAMES, run);

    expect(called.install).not.toHaveBeenCalled();
    expect(outcome.key).toEqual({ kind: "unset", why: "You cancelled. No new key was set." });
    expect(outcome.servers).toHaveLength(2);
  });

  it("asks for no key when no Server activated", async () => {
    answering();
    const refused: ServerActivation = { id: WEB_1, name: "web-1", kind: "refused", why: "It didn't answer." };
    const { called, run } = steps({ servers: [refused] });

    expect(await runActivation(OFFER, NAMES, run)).toEqual({ servers: [refused], key: null });
    expect(called.generate).not.toHaveBeenCalled();
  });

  it("sets no key when no Server made one, in Cloud's words", async () => {
    answering();
    const { called, run } = steps({ generate: () => Promise.reject(new Error("No server reports this activation finished. Nothing changed.")) });

    const outcome = await runActivation(OFFER, NAMES, run);

    expect(outcome.key).toEqual({ kind: "unset", why: "No server reports this activation finished. Nothing changed. No new key was set." });
    expect(called.reviews).toHaveLength(1);
  });
});
