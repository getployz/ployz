/**
 * A person's Save from the browser, which signs the Environment's automatic deploy rule in the same tap when what it
 * saves isn't what the kept rule approves. The browser drafts the rule as the Source step of one bundle with the
 * ployz-core code the Servers check, the person reads it, and the Store keeps it in the Save's own transaction. Cloud
 * only relays the rule view and the answer, and each Server checks the rule on every push.
 */
import type { ConfigCommand, PostedVersion, RuleView, SavedRule, VersionsDraft } from "@ployz/sdk";
import { type PushBranch, sourceNeed } from "@ployz/sdk/authority";
import { StoreRefused } from "#/lib/refusal";
import type { OnlyPasskey } from "#/modules/passkeys/passkey-statement";
import { signWithPasskey } from "#/modules/passkeys/passkey-statement";
import { namesReview } from "./deploy-tap";
import type { SaveTap } from "./deploy-tap.server";
import { draftSource, movedFrom, type OwnerOf, priorOf } from "./source-step";

/** A Save as the Store takes it. */
export type Publish = Extract<ConfigCommand, { command: "publish" }>;

/** How long a bundle stays open after the browser drafts it. */
const WINDOW_MS = 10 * 60_000;

/** What the sheet asks the person to sign before the Save, and the Save it sends with their answer. */
export type SaveAsk = {
  readonly draft: VersionsDraft & { readonly source: NonNullable<VersionsDraft["source"]> };
  readonly passkeys: readonly OnlyPasskey[];
  readonly command: Publish;
  readonly prior: SavedRule["prior"];
  /** The branch whose pushes stop deploying when the rule moves off it. */
  readonly stops: string | null;
  /** The review version the rule was drafted from: only a retry of this review reuses the answer. */
  readonly review: string;
};

/**
 * `held` is why a Save that pushes need a new rule for goes without one: pushes wait for approval until one is signed.
 * `unchecked` marks a Save whose Servers Cloud couldn't compare with the rule's: only a push placed on a Server the rule
 * doesn't name waits.
 */
export type SavePrepared =
  | { readonly kind: "save"; readonly command: Publish; readonly held?: string; readonly unchecked?: true }
  | { readonly kind: "refused"; readonly message: string }
  | { readonly kind: "ask"; readonly ask: SaveAsk };

/** Why a stage-only member's Save holds pushes. */
const ASK_DEPLOYER = "Ask someone who can deploy to approve the rule.";
/** Why the switch can't turn the rule on for this person here. */
const NOT_HERE = "An owner or admin turns on automatic deploys here.";
/** Why the switch signs nothing when no Server checks approvals. */
const NO_CHECK = "None of your servers checks approvals, so pushes deploy without a rule.";
/** Why the switch signs nothing once someone else changed the Environment. */
const MOVED = "This environment changed since the page loaded. Reload.";

/** Cloud's read of what a Save saves; with `gate`, also what asking for a passkey needs. */
export type ReadSave = (gate: boolean) => Promise<SaveTap>;

const READ = "This browser couldn't read the rule.";


/** Whether `command` saves the review `view` was read at. */
const saves = (view: RuleView, command: Publish): view is RuleView & { version: string } =>
  view.version !== undefined && (command.version === null || command.version === undefined || namesReview(view.version, command.version));

/**
 * What `command` needs before it is sent: nothing, a passkey's answer to the rule it should sign, or why the staged read
 * failed. Whether a rule is needed is read from the view alone, so a Save that needs none never reaches a Server or asks
 * for a passkey. When one is needed but can't be asked for, the Save goes without it and says why. A Save whose review
 * moved since the person looked is sent as it is, and the Store refuses it as stale.
 *
 * `on` is the branch Automatic deploys turns the rule on for or approves again: that Save always signs a rule, even one
 * the person ended, and saves nothing when it can't.
 */
export async function prepareSave(command: Publish, read: ReadSave, ownerOf: OwnerOf, now = Date.now(), on?: PushBranch): Promise<SavePrepared> {
  const plain = { kind: "save", command } as const;
  // Automatic deploys asked for the rule: anything short of signing it saves nothing.
  const skip = (reason: string): SavePrepared => on === undefined ? plain : { kind: "refused", message: reason };
  const first = await read(false);
  if (first.kind === "refused") return first;
  if (first.kind === "off") return skip(NOT_HERE);
  if (!saves(first.view, command)) return skip(MOVED);
  let need: ReturnType<typeof sourceNeed>;
  try {
    need = on === undefined ? sourceNeed(first.view) : "rule";
  } catch (error) {
    const reason = error instanceof Error && error.message !== "" ? error.message : READ;
    return on === undefined ? { ...plain, held: reason } : skip(reason);
  }
  if (need === "none") return plain;
  // Someone who can't deploy signs nothing: say who can.
  if (first.kind === "unsigned") return on !== undefined ? skip(ASK_DEPLOYER) : need === "rule" ? { ...plain, held: ASK_DEPLOYER } : plain;
  // A new rule holds every push. When only the Servers the kept rule names may be out of date, a push holds only if it
  // lands on one the rule doesn't name, which is certain once the Cluster's Servers drafted a rule that names more.
  const held = (reason: string, drafted = false): SavePrepared =>
    on !== undefined ? skip(reason)
    : need === "servers" && !drafted ? { ...plain, held: reason, unchecked: true } : { ...plain, held: reason };
  const second = await read(true).catch(() => null);
  if (second === null) return held("Couldn't reach Ployz Cloud.");
  // Publish checks the version and who may save; a refused read only keeps the rule from being asked for.
  if (second.kind === "refused") return held(second.message);
  if (second.kind !== "view") return skip(READ);
  const { view, tap: gate } = second;
  if (!saves(view, command)) return skip(MOVED);
  if (gate === null || gate.kind === "off") return skip(NO_CHECK);
  if (gate.kind === "refused") return held(gate.message);
  const drafted = await draftSource({
    cluster: gate.cluster, namespace: view.namespace, volumes: view.volumes, services: [],
    operation: crypto.randomUUID().replaceAll("-", ""), issued_at: now, expires_at: now + WINDOW_MS,
    seed: [...crypto.getRandomValues(new Uint8Array(16))], against: gate.against,
  }, view, gate.members, ownerOf, on);
  if (drafted.kind === "held") return held(drafted.reason, drafted.drafted);
  const signed = drafted.draft;
  if (signed?.source === undefined) return on === undefined ? plain : skip(`No service deploys on push from ${on.branch}.`);
  return {
    kind: "ask",
    ask: {
      draft: { ...signed, source: signed.source }, passkeys: gate.passkeys, review: view.version,
      command: { ...command, version: command.version ?? view.version }, prior: priorOf(view),
      stops: movedFrom(view, signed.source),
    },
  };
}

/** `ask`'s rule, answered by the person's passkey as the one step of its bundle, as the Save carries it. */
export async function signRule(ask: SaveAsk): Promise<SavedRule> {
  const proposal = await signWithPasskey(ask.draft.bundle, "workflow", ask.passkeys);
  const rule: PostedVersion["statement"] = { body: ask.draft.source.body, signatures: [], proposal };
  return { rule, births: ask.draft.source.births ?? {}, prior: ask.prior };
}

/** A rule answered for review `review` whose Save the Store asked to confirm: the confirmed retry carries it. */
export type Answered = { readonly review: string; readonly rule: SavedRule };

/** How a Save is sent, and where the Save's answer waits for a confirmed retry. */
export type SaveSteps = {
  readonly answered: { current: Answered | null };
  readonly commit: (command: Publish, handles: readonly string[]) => Promise<void>;
  readonly prepare: (command: Publish, on?: PushBranch) => Promise<SavePrepared>;
  /** Asks the person to sign `ask`'s rule; settles once their Save was sent, or throws why it wasn't. */
  readonly ask: (ask: SaveAsk, handles: readonly string[]) => Promise<void>;
  /** Tells the person the Save went without a rule, and why; `unchecked` when pushes may not wait. */
  readonly held: (reason: string, unchecked: boolean) => void;
  /** Tells the person why nothing was saved; the Save then throws. */
  readonly refused: (message: string) => Error;
};

/** Sends `command` with `rule`, keeping the answer when the Store asks to confirm with one of `handles`. */
export async function sendRule(steps: Pick<SaveSteps, "answered" | "commit">, command: Publish, answered: Answered, handles: readonly string[]) {
  try {
    await steps.commit({ ...command, rule: answered.rule }, handles);
  } catch (error) {
    // The Store asks again (Servers hold more by now): the caller's next accepted retry still needs no tap.
    if (error instanceof StoreRefused && handles.includes(error.code)) steps.answered.current = answered;
    throw error;
  }
}

/**
 * Saves `command`, turning the rule on for `on` as [`prepareSave`] does. A confirmed retry of the review a rule was
 * answered for carries that rule and the rule it read as kept, so the Store refuses the whole Save if the kept rule moved
 * since, rather than saving without it.
 */
export async function saveWith(steps: SaveSteps, command: Publish, handles: readonly string[] = [], on?: PushBranch): Promise<void> {
  const answered = steps.answered.current;
  steps.answered.current = null;
  if (answered !== null && command.version !== null && command.version !== undefined && namesReview(answered.review, command.version)) {
    return sendRule(steps, command, answered, handles);
  }
  const prepared = await steps.prepare(command, on);
  if (prepared.kind === "refused") throw steps.refused(prepared.message);
  if (prepared.kind === "ask") return steps.ask(prepared.ask, handles);
  await steps.commit(prepared.command, handles);
  if (prepared.held !== undefined) steps.held(prepared.held, prepared.unchecked === true);
}
