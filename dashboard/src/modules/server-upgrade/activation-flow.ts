/**
 * Activation as this browser asks for it: two passkey prompts, each answering a body the ployz-core code the Servers
 * run drafted, with every line read back from that body. The first answers the starting set over the Servers Cloud
 * listed; the second names the recipient key one of those Servers generated. A browser never generates the key, never
 * signs as a Device, and never sees a recovery code.
 *
 * Cloud relays the Servers and the key. What it relays shows, but each Server checks both bodies against its own
 * trusted snapshot, so a set that names a Server it has no evidence for is refused there.
 */
import { sealingRotationBody, startingSetBody } from "@ployz/sdk/authority";
import type { MachineId, OperationId } from "@ployz/sdk";
import { Schema } from "effect";
import { toErrorMessage } from "#/lib/error-message";
import { shownText } from "#/modules/github/preview-review";
import { utc } from "#/modules/approval-session/approval-session";
import type { Review } from "#/modules/approval-session/approval-prompt";
import { PasskeyRefused, signWithPasskey } from "#/modules/approval-session/passkey-statement";
import type { ActivationMember, Generated, PasskeyAnswered, RecipientInstall, ServerActivation } from "./activation";

/** Why nothing was activated, in words the page shows as they are. */
export class ActivationRefused extends Error {}

/** A random operation ID, as `OperationId::random` writes one: 32 lowercase hex digits. */
// SAFETY: a UUID without its dashes is 32 lowercase hex characters, the form OperationId parses.
const operationId = () => crypto.randomUUID().replaceAll("-", "") as OperationId;

const Member = Schema.Struct({ id: Schema.String, public_key: Schema.Array(Schema.Int), subnet: Schema.String });
const SetBody = Schema.fromJsonString(Schema.Struct({
  operation: Schema.String,
  members: Schema.Array(Member),
  issued_at: Schema.Number,
  expires_at: Schema.Number,
}));

/** The Servers a body names, as ployz-core reads them again. */
const asMembers = (members: readonly ActivationMember[]) =>
  // SAFETY: each Member is a Server's own Inspect answer or a drafted body's; ployz-core parses every field again.
  members.map((member) => ({ id: member.id as MachineId, public_key: [...member.public_key], subnet: member.subnet }));

async function answered(body: string, refusedWith: string): Promise<PasskeyAnswered> {
  try {
    const signed = await signWithPasskey(body, "enforce");
    const [assertion] = signed.assertions ?? [];
    if (assertion === undefined) throw new PasskeyRefused("Your passkey didn't answer.");
    return { body: signed.body, signatures: [], assertions: [assertion] };
  } catch (error) {
    if (error instanceof PasskeyRefused) throw new ActivationRefused(`${error.message} ${refusedWith}`);
    if (error instanceof Error && error.name === "NotAllowedError") throw new ActivationRefused(`Your passkey didn't answer. ${refusedWith}`);
    throw new ActivationRefused(`This browser couldn't use a passkey. ${refusedWith}`);
  }
}

const NOTHING_ACTIVATED = "Nothing was activated.";

/**
 * Prompt 1: the starting set over `members`, answered by the person's passkey once they approve every line of it.
 * Throws an `ActivationRefused` when they cancel, the passkey doesn't answer, or the approval expires first.
 */
export async function approveStartingSet(
  members: readonly ActivationMember[],
  names: Readonly<Record<string, string>>,
  review: (review: Review) => Promise<boolean>,
  now = Date.now(),
): Promise<PasskeyAnswered> {
  let drafted: { body: string; lines: string[] };
  try {
    drafted = startingSetBody(
      { operation: operationId(), members: asMembers(members), issued_at: now, signer: "passkey" },
      { servers: { ...names } },
    );
  } catch {
    throw new ActivationRefused(`This browser couldn't draft the list of servers. ${NOTHING_ACTIVATED}`);
  }
  const terms = Schema.decodeUnknownSync(SetBody)(drafted.body);
  const approved = await review({
    title: "Turn on device enforcement?",
    lines: [
      ...drafted.lines,
      `  activation ${shownText(terms.operation)}`,
      "  signer your passkey",
      `  valid ${shownText(utc(terms.issued_at))} to ${shownText(utc(terms.expires_at))}`,
    ],
    note: "It can't be turned off.",
    action: "Activate",
  });
  if (!approved) throw new ActivationRefused(`You cancelled. ${NOTHING_ACTIVATED}`);
  if (Date.now() >= terms.expires_at) throw new ActivationRefused(`Your approval expired. ${NOTHING_ACTIVATED}`);
  return answered(drafted.body, NOTHING_ACTIVATED);
}

/** What a rotation names besides its key: the Cluster, the activation it follows and the Servers that get the key. */
export type RotationScope = { readonly cluster: string; readonly operation: string; readonly members: readonly ActivationMember[] };

const NO_NEW_KEY = "No new key was set.";

/**
 * Prompt 2: the rotation naming `recipient`, for the Members of `scope`. Throws an `ActivationRefused` when the person
 * cancels or the passkey doesn't answer.
 */
export async function approveRotation(
  scope: RotationScope,
  recipient: string,
  names: Readonly<Record<string, string>>,
  review: (review: Review) => Promise<boolean>,
): Promise<PasskeyAnswered> {
  let drafted: { body: string; lines: string[] };
  try {
    drafted = sealingRotationBody({
      cluster: scope.cluster,
      // SAFETY: an operation a Server reported or this browser drafted; ployz-core parses it again here.
      operation: scope.operation as OperationId,
      recipient,
      members: asMembers(scope.members),
      signer: "passkey",
    }, { servers: { ...names } });
  } catch {
    throw new ActivationRefused(`This browser couldn't draft the new key. ${NO_NEW_KEY}`);
  }
  const approved = await review({
    title: "Seal new values to this key?",
    lines: [...drafted.lines, "  signer your passkey"],
    note: "One of your servers made this key.",
    action: "Use this key",
  });
  if (!approved) throw new ActivationRefused(`You cancelled. ${NO_NEW_KEY}`);
  return answered(drafted.body, NO_NEW_KEY);
}

/** Where a run stands, for the page to say while it waits. */
export type ActivationStep = "approving" | "activating" | "generating" | "installing";

/** What a run calls: the person's review, and Cloud's three steps. */
export type ActivationSteps = {
  readonly review: (review: Review) => Promise<boolean>;
  readonly activate: (startingSet: PasskeyAnswered) => Promise<readonly ServerActivation[]>;
  readonly generate: (operation: string) => Promise<Generated>;
  readonly install: (rotation: PasskeyAnswered, generator: string) => Promise<readonly RecipientInstall[]>;
  readonly step: (step: ActivationStep) => void;
};

/** What became of the new key: installed where each Server says, or why none was set. */
export type KeyOutcome =
  | { readonly kind: "installed"; readonly installs: readonly RecipientInstall[] }
  | { readonly kind: "unset"; readonly why: string };

export type ActivationOutcome = { readonly servers: readonly ServerActivation[]; readonly key: KeyOutcome | null };

/**
 * The new key after activation: a Server generates it, the person approves it with their passkey, then each Server
 * installs it. Never throws: a step that doesn't finish leaves the key unset, and the page offers to set it again.
 */
export async function setNewKey(
  scope: RotationScope,
  names: Readonly<Record<string, string>>,
  steps: ActivationSteps,
): Promise<KeyOutcome> {
  try {
    steps.step("generating");
    const { generator, recipient } = await steps.generate(scope.operation);
    steps.step("approving");
    const rotation = await approveRotation(scope, recipient, names, steps.review);
    steps.step("installing");
    return { kind: "installed", installs: await steps.install(rotation, generator) };
  } catch (error) {
    return { kind: "unset", why: error instanceof ActivationRefused ? error.message : `${toErrorMessage(error, "Ployz Cloud didn't answer.")} ${NO_NEW_KEY}` };
  }
}

/**
 * The whole activation: prompt 1 over the Servers the gate offered, each Server activated in turn, then the new key
 * for the Servers the answered set names. Throws an `ActivationRefused` when nothing was activated.
 */
export async function runActivation(
  offer: { readonly cluster: string; readonly members: readonly ActivationMember[] },
  names: Readonly<Record<string, string>>,
  steps: ActivationSteps,
): Promise<ActivationOutcome> {
  steps.step("approving");
  const startingSet = await approveStartingSet(offer.members, names, steps.review);
  steps.step("activating");
  let servers: readonly ServerActivation[];
  try {
    servers = await steps.activate(startingSet);
  } catch (error) {
    throw new ActivationRefused(toErrorMessage(error, "Ployz Cloud didn't answer."));
  }
  if (!servers.some((server) => server.kind === "activated")) return { servers, key: null };
  const set = Schema.decodeUnknownSync(SetBody)(startingSet.body);
  return { servers, key: await setNewKey({ cluster: offer.cluster, operation: set.operation, members: set.members }, names, steps) };
}
