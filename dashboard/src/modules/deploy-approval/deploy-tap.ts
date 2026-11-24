/**
 * A person's Deploy from the browser, signed with one passkey prompt when its Servers need it. The Store's plan names
 * each Service a Server would run only signed (`unsigned`). The browser drafts one version of each with the ployz-core
 * code the Servers check, and the automatic deploy rule as a Source step when what the Deploy saves isn't what the kept
 * rule approves, shows the lines it signs, and the passkey answers one bundle of them. Cloud only relays the plan, the
 * tag pins and the answer, and each Server checks every step.
 */
import type { Needs, PostedVersion, SavedRule, Unsigned, VersionsDraft } from "@ployz/sdk";
import { Option, Schema } from "effect";
import { versionSteps } from "@ployz/sdk/authority";
import type { AdmitDeploy } from "#/modules/config-store/admit-deploy";
import { type OnlyPasskey, signWithPasskey } from "#/modules/passkeys/passkey-statement";
import { LOSS_MARK } from "#/modules/volume-placement/volume-placement.review";
import type { DeployPlan } from "./deploy-tap.server";
import { draftSource, type OwnerOf, priorOf } from "./source-step";

/** What the Store refuses a plan with while a copied sealed value, such as a Database Preset's password, has no value yet. */
export const Pending = Schema.Struct({
  environment: Schema.String,
  pending: Schema.NonEmptyArray(Schema.Struct({ service: Schema.String, id: Schema.String, key: Schema.String, generated: Schema.String })),
});
export const isPending = Schema.is(Pending);

/**
 * `version` past the one edit that wrote `written`: a review version is `REVISION:SAVED:TOKEN…`, and the edit moved only
 * the Working revision, by one. Anything else stays as it was, and the plan calls it stale.
 */
export function moved(version: string | null | undefined, written: number | null) {
  const revision = /^(\d+):/u.exec(version ?? "")?.[1];
  if (version === null || version === undefined || revision === undefined || written !== Number(revision) + 1) return version;
  return `${written}${version.slice(revision.length)}`;
}

const decodeBody = Schema.decodeUnknownOption(Schema.fromJsonString(Schema.Struct({ service: Schema.String })));

/** The Service a posted version signs, or null for a body that doesn't read: the Store refuses that one itself. */
export const versionService = (version: PostedVersion): string | null =>
  Option.getOrNull(Option.map(decodeBody(version.statement.body), ({ service }) => service));

/** How long a bundle stays open after the browser drafts it. */
const WINDOW_MS = 10 * 60_000;

/**
 * Whether `given`, a caller's version, names the review the Store versions `current`, as the Store reads it: that
 * version, or it followed by `:` and an approval digest, either one bound to a loss with `#loss-`.
 */
export function namesReview(current: string, given: string) {
  const reviewed = given.split(LOSS_MARK)[0] ?? given;
  return reviewed === current || reviewed.startsWith(`${current}:`);
}

const BLOCKED = {
  rule: (service) => `No automatic deploy rule covers ${service}. Approve one, or run ployz deploy.`,
  rebuild: (service) => `Push to ${service}'s branch first. Its rule hasn't built this version yet.`,
  digest: (service) => `${service} pulls a private tag. Write its image by digest (image@sha256:…).`,
  ended: (service) => `Automatic deploys are off for ${service}. Turn on Deploy every push to deploy it.`,
} satisfies Record<Exclude<Needs, "version">, (service: string) => string>;

/** Why a Service no passkey can sign keeps the Deploy from asking, or null when a passkey can sign each of `unsigned`. */
export function blocked(unsigned: readonly Unsigned[]) {
  for (const { service, needs } of unsigned) if (needs !== "version") return BLOCKED[needs](service);
  return null;
}

/** What the sheet asks the person to sign, and the Deploy it admits with their answer. */
export type Ask = {
  readonly namespace: string;
  readonly services: readonly string[];
  readonly draft: VersionsDraft;
  readonly passkeys: readonly OnlyPasskey[];
  /** The Deploy, at the caller's version, or at the plan's when the caller named none. */
  readonly command: AdmitDeploy;
  /** The Store's plan version the draft signs: only a Volume-loss retry of this plan reuses the answer. */
  readonly plan: string;
  /** The rule the Store kept when the draft read it, which a Deploy that signs another names. */
  readonly prior: SavedRule["prior"];
  readonly held: Held | null;
};

/** Why a Deploy goes without the automatic deploy rule it needs, so pushes wait; `unchecked` when they may not. */
export type Held = { readonly reason: string; readonly unchecked: boolean };

export type Prepared =
  | { readonly kind: "admit"; readonly command: AdmitDeploy; readonly held: Held | null }
  | { readonly kind: "stale" }
  | { readonly kind: "refused"; readonly message: string }
  | { readonly kind: "ask"; readonly ask: Ask };

/**
 * What `command` needs before it is admitted: nothing, a passkey's answer to the drafted versions and rule, or a reason
 * it can't ask. Nothing prompts here, and a reason is given before any prompt. A rule the tap can't sign never stops
 * the Deploy: it goes without it, and `held` says why. Every exit admits at the caller's version as given, `#loss-` mark
 * and digest included, or at the plan's when the caller named none.
 */
export async function prepareDeploy(
  command: AdmitDeploy,
  read: (command: AdmitDeploy) => Promise<DeployPlan>,
  /** How the rule names its repository's owner; a caller that never reads the rule passes none. */
  ownerOf?: OwnerOf,
  now = Date.now(),
): Promise<Prepared> {
  const planned = await read(command);
  if (planned.kind === "refused") return planned;
  const { plan, tap: gate, source } = planned;
  if (command.version !== null && command.version !== undefined && !namesReview(plan.version, command.version)) return { kind: "stale" };
  const bound: AdmitDeploy = { ...command, version: command.version ?? plan.version };
  let held: Held | null = source?.kind === "held" ? { reason: source.reason, unchecked: source.unchecked } : null;
  const admit = (): Prepared => ({ kind: "admit", command: bound, held });
  // A version the Deploy already carries, such as one a CLI's device key signed, covers its Service: only the rest ask.
  const covered = new Set((command.versions ?? []).map(versionService));
  const unsigned = (plan.unsigned ?? []).filter((entry) => !covered.has(entry.service));
  const rule = source?.kind === "sign" && gate.kind === "ready" && ownerOf !== undefined ? source : null;
  if (gate.kind === "off" || (unsigned.length === 0 && rule === null)) return admit();
  if (gate.kind === "refused") return gate;
  const refused = blocked(unsigned);
  if (refused !== null) return { kind: "refused", message: refused };
  const names = new Set(unsigned.map((entry) => entry.service));
  const specs = (plan.target ?? []).filter((spec) => names.has(spec.name));
  const asked = {
    cluster: gate.cluster, namespace: plan.namespace, volumes: plan.volumes, services: specs, pins: gate.pins,
    operation: crypto.randomUUID().replaceAll("-", ""), issued_at: now, expires_at: now + WINDOW_MS,
    seed: [...crypto.getRandomValues(new Uint8Array(16))], against: gate.against,
  };
  let draft: VersionsDraft | null = null;
  try {
    const drafted = rule === null || ownerOf === undefined ? null : await draftSource(asked, rule.view, gate.members, ownerOf);
    if (drafted?.kind === "drafted") draft = drafted.draft;
    else {
      // Without the rule, only a push placed on a Server the kept rule doesn't name waits, unless the draft named more.
      if (drafted?.kind === "held") held = { reason: drafted.reason, unchecked: rule !== null && rule.unchecked && !drafted.drafted };
      if (specs.length > 0) draft = versionSteps(asked);
    }
  } catch (error) {
    return { kind: "refused", message: error instanceof Error ? error.message : "This browser couldn't draft the deploy." };
  }
  if (draft === null) return admit();
  return {
    kind: "ask",
    ask: {
      namespace: plan.namespace, services: draft.steps.map((step) => step.service), draft, passkeys: gate.passkeys,
      command: bound, plan: plan.version, prior: rule === null ? null : priorOf(rule.view), held,
    },
  };
}

/** A passkey's answer to an ask: each version, and the rule when the bundle signs one. */
export type Answer = { readonly versions: PostedVersion[]; readonly rule?: SavedRule };

/** `ask`'s versions and rule, each a step of the one bundle the person's passkey answers, as the Deploy posts them. */
export async function signVersions(ask: Ask): Promise<Answer> {
  const proposal = await signWithPasskey(ask.draft.bundle, "workflow", ask.passkeys);
  const versions = ask.draft.steps.map((step) => {
    const posted: PostedVersion = { statement: { body: step.body, signatures: [], proposal }, births: step.births };
    if (step.tag !== undefined) posted.pin = { tag: step.tag, image: step.image };
    return posted;
  });
  const source = ask.draft.source;
  if (source === undefined) return { versions };
  return { versions, rule: { rule: { body: source.body, signatures: [], proposal }, births: source.births ?? {}, prior: ask.prior } };
}

/** `command` carrying `answer`. */
export const answered = (command: AdmitDeploy, answer: Answer): AdmitDeploy =>
  answer.rule === undefined ? { ...command, versions: answer.versions } : { ...command, versions: answer.versions, rule: answer.rule };
