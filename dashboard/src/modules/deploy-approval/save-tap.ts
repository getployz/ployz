/**
 * A person's Save from the browser, which signs the Environment's automatic deploy rule in the same tap when what it
 * saves isn't what the kept rule approves. The browser drafts the rule as the Source step of one bundle with the
 * ployz-core code the Servers check, the person reads it, and the Store keeps it in the Save's own transaction. Cloud
 * only relays the rule view and the answer, and each Server checks the rule on every push.
 */
import type { AutoDeployRule, ConfigCommand, PostedVersion, SavedRule, VersionsDraft } from "@ployz/sdk";

/** A Save as the Store takes it. */
export type Publish = Extract<ConfigCommand, { command: "publish" }>;
import { versionSteps } from "@ployz/sdk/authority";
import type { OnlyPasskey } from "#/modules/passkeys/passkey-statement";
import { signWithPasskey } from "#/modules/passkeys/passkey-statement";
import { namesReview } from "./deploy-tap";
import type { SaveTap } from "./deploy-tap.server";

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

export type SavePrepared =
  | { readonly kind: "save"; readonly command: Publish }
  | { readonly kind: "refused"; readonly message: string }
  | { readonly kind: "ask"; readonly ask: SaveAsk };

/** The repository owner's GitHub ID, or a reason the rule can't name it. */
export type OwnerOf = (repository: string, repositoryId: number) => Promise<{ ownerId: number } | { problem: string }>;

const parsed = (body: string): AutoDeployRule => {
  // SAFETY: ployz-core drafted or the Store kept this body as an `AutoDeployRule`.
  return JSON.parse(body) as AutoDeployRule;
};

/**
 * What `command` needs before it is sent: nothing, a passkey's answer to the rule it should sign, or a reason it can't
 * ask. A Save whose review moved since the person looked is sent as it is, and the Store refuses it as stale.
 */
export async function prepareSave(command: Publish, read: () => Promise<SaveTap>, ownerOf: OwnerOf, now = Date.now()): Promise<SavePrepared> {
  const tap = await read();
  if (tap.kind === "off") return { kind: "save", command };
  if (tap.kind === "refused") return tap;
  const { view, tap: gate } = tap;
  if (view.version === undefined || (command.version !== null && command.version !== undefined && !namesReview(view.version, command.version))) {
    return { kind: "save", command };
  }
  const operation = crypto.randomUUID().replaceAll("-", "");
  const seed = [...crypto.getRandomValues(new Uint8Array(16))];
  const draft = (ownerId: number) => versionSteps({
    cluster: gate.cluster, namespace: view.namespace, volumes: view.volumes, services: [], operation,
    issued_at: now, expires_at: now + WINDOW_MS, seed, against: gate.against,
    source: { rule: view.rule ?? null, sources: view.sources, target: view.target, machines: [...gate.members], owner_id: ownerId },
  });
  let first: VersionsDraft | null;
  try {
    first = draft(0);
  } catch (error) {
    return { kind: "refused", message: error instanceof Error ? error.message : "This browser couldn't draft the rule." };
  }
  if (first?.source === undefined) return { kind: "save", command };
  const { repository, repository_id } = parsed(first.source.body).source;
  const owner = await ownerOf(repository, repository_id);
  if ("problem" in owner) return { kind: "refused", message: owner.problem };
  const signed = draft(owner.ownerId);
  if (signed?.source === undefined) return { kind: "save", command };
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
