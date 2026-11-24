/**
 * A person's Save from the browser, which signs the Environment's automatic deploy rule in the same tap when what it
 * saves isn't what the kept rule approves. The browser drafts the rule as the Source step of one bundle with the
 * ployz-core code the Servers check, the person reads it, and the Store keeps it in the Save's own transaction. Cloud
 * only relays the rule view and the answer, and each Server checks the rule on every push.
 */
import type { AutoDeployRule, ConfigCommand, PostedVersion, RuleView, SavedRule, VersionsDraft } from "@ployz/sdk";
import { sourceNeed, versionSteps } from "@ployz/sdk/authority";
import { StoreRefused } from "#/lib/refusal";
import type { OnlyPasskey } from "#/modules/passkeys/passkey-statement";
import { signWithPasskey } from "#/modules/passkeys/passkey-statement";
import { namesReview } from "./deploy-tap";
import type { SaveTap } from "./deploy-tap.server";

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
  /** The review version the rule was drafted from: only a retry of this review reuses the answer. */
  readonly review: string;
};

/** `held` is why a Save that pushes need a new rule for goes without one: pushes wait for approval until one is signed. */
export type SavePrepared =
  | { readonly kind: "save"; readonly command: Publish; readonly held?: string }
  | { readonly kind: "refused"; readonly message: string }
  | { readonly kind: "ask"; readonly ask: SaveAsk };

/** The repository owner's GitHub ID, or a reason the rule can't name it. */
export type OwnerOf = (repository: string, repositoryId: number) => Promise<{ ownerId: number } | { problem: string }>;

/** Cloud's read of what a Save saves; with `gate`, also what asking for a passkey needs. */
export type ReadSave = (gate: boolean) => Promise<SaveTap>;

const parsed = (body: string): AutoDeployRule => {
  // SAFETY: ployz-core drafted or the Store kept this body as an `AutoDeployRule`.
  return JSON.parse(body) as AutoDeployRule;
};

/** Whether `command` saves the review `view` was read at. */
const saves = (view: RuleView, command: Publish): view is RuleView & { version: string } =>
  view.version !== undefined && (command.version === null || command.version === undefined || namesReview(view.version, command.version));

/**
 * What `command` needs before it is sent: nothing, a passkey's answer to the rule it should sign, or why the staged read
 * failed. Whether a rule is needed is read from the view alone, so a Save that needs none never reaches a Server or asks
 * for a passkey. When one is needed but can't be asked for, the Save goes without it and says why. A Save whose review
 * moved since the person looked is sent as it is, and the Store refuses it as stale.
 */
export async function prepareSave(command: Publish, read: ReadSave, ownerOf: OwnerOf, now = Date.now()): Promise<SavePrepared> {
  const plain = { kind: "save", command } as const;
  const first = await read(false);
  if (first.kind !== "view") return first.kind === "off" ? plain : first;
  if (!saves(first.view, command)) return plain;
  let need: ReturnType<typeof sourceNeed>;
  try {
    need = sourceNeed(first.view);
  } catch (error) {
    return { ...plain, held: error instanceof Error && error.message !== "" ? error.message : "This browser couldn't read the rule." };
  }
  if (need === "none") return plain;
  // Only the Servers the kept rule names may be out of date: its pushes still deploy, so nothing is held.
  const held = (reason: string): SavePrepared => need === "rule" ? { ...plain, held: reason } : plain;
  const second = await read(true).catch(() => null);
  if (second === null) return held("Couldn't reach Ployz Cloud.");
  if (second.kind !== "view") return second.kind === "off" ? plain : second;
  const { view, tap: gate } = second;
  if (!saves(view, command) || gate === null || gate.kind === "off") return plain;
  if (gate.kind === "refused") return held(gate.message);
  const operation = crypto.randomUUID().replaceAll("-", "");
  const seed = [...crypto.getRandomValues(new Uint8Array(16))];
  const draft = (ownerId: number) => versionSteps({
    cluster: gate.cluster, namespace: view.namespace, volumes: view.volumes, services: [], operation,
    issued_at: now, expires_at: now + WINDOW_MS, seed, against: gate.against,
    source: { rule: view.rule ?? null, sources: view.sources, target: view.target, machines: [...gate.members], owner_id: ownerId },
  });
  let signed: VersionsDraft | null;
  try {
    const unowned = draft(0);
    if (unowned?.source === undefined) return plain;
    const { repository, repository_id } = parsed(unowned.source.body).source;
    const owner = await ownerOf(repository, repository_id);
    if ("problem" in owner) return held(owner.problem);
    signed = draft(owner.ownerId);
  } catch (error) {
    return held(error instanceof Error && error.message !== "" ? error.message : "This browser couldn't draft the rule.");
  }
  if (signed?.source === undefined) return plain;
  const kept = view.rule === null || view.rule === undefined ? null : parsed(view.rule.body);
  return {
    kind: "ask",
    ask: {
      draft: { ...signed, source: signed.source }, passkeys: gate.passkeys, review: view.version,
      command: { ...command, version: command.version ?? view.version },
      prior: kept === null ? null : { rule: kept.rule, generation: kept.generation },
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
  readonly prepare: (command: Publish) => Promise<SavePrepared>;
  /** Asks the person to sign `ask`'s rule; settles once their Save was sent, or throws why it wasn't. */
  readonly ask: (ask: SaveAsk, handles: readonly string[]) => Promise<void>;
  /** Tells the person the Save went without a rule, and why. */
  readonly held: (reason: string) => void;
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
 * Saves `command`. A confirmed retry of the review a rule was answered for carries that rule and the rule it read as
 * kept, so the Store refuses the whole Save if the kept rule moved since, rather than saving without it.
 */
export async function saveWith(steps: SaveSteps, command: Publish, handles: readonly string[] = []): Promise<void> {
  const answered = steps.answered.current;
  steps.answered.current = null;
  if (answered !== null && command.version !== null && command.version !== undefined && namesReview(answered.review, command.version)) {
    return sendRule(steps, command, answered, handles);
  }
  const prepared = await steps.prepare(command);
  if (prepared.kind === "refused") throw steps.refused(prepared.message);
  if (prepared.kind === "ask") return steps.ask(prepared.ask, handles);
  await steps.commit(prepared.command, handles);
  if (prepared.held !== undefined) steps.held(prepared.held);
}
