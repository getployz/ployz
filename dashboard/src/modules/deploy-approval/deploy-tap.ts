/**
 * A person's Deploy from the browser, signed with one passkey prompt when its Servers need it. The Store's plan names
 * each Service a Server would run only signed (`unsigned`). The browser drafts one version of each with the ployz-core
 * code the Servers check, and the automatic deploy rule as a Source step when what the Deploy saves isn't what the kept
 * rule approves, shows the lines it signs, and the passkey answers one bundle of them. Cloud only relays the plan, the
 * tag pins and the answer, and each Server checks every step.
 */
import type { PlanView, PostedVersion, SavedRule, Unsigned, VersionsDraft } from "@ployz/sdk";
import { Option, Schema } from "effect";
import { versionSteps } from "@ployz/sdk/authority";
import { StoreRefused } from "#/lib/refusal";
import type { AdmitDeploy } from "#/modules/config-store/admit-deploy";
import { environmentKey } from "#/modules/config-store/environment-ref";
import { type OnlyPasskey, signWithPasskey } from "#/modules/passkeys/passkey-statement";
import { LOSS_MARK } from "#/modules/volume-placement/volume-placement.review";
import type { DeployPlan, DeploySource, DeployTap } from "./deploy-tap.server";
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

/**
 * Whether `entry`'s Service builds from source under the automatic deploy rule, so no passkey signs a version of it. The
 * Deploy's tap may sign the rule with it, and each Server checks the build against the rule that stands.
 */
export const underRule = ({ needs, ended }: Unsigned) => (needs === "rule" || needs === "rebuild") && ended !== true;

/**
 * The automatic deploy rule a Deploy carries to the Store: none, so the live rule stands, or one a passkey signed, with
 * the Services it approves, null when it isn't the Source step of the bundle the passkey signed.
 */
export type CarriedRule = { readonly kind: "none" } | { readonly kind: "signed"; readonly sourced: ReadonlySet<string> | null };

/**
 * Whether a Deploy goes to the Store as it is: `admit`, with `source` when a Source step the tap can sign builds a
 * Service it ships; `ask` when a passkey must sign it first, with what to tell someone who sends it unsigned; `refused`
 * when no tap can sign it, `unreached` when Cloud couldn't reach the Servers to ask.
 */
export type Admission =
  | { readonly kind: "admit"; readonly source: boolean }
  | { readonly kind: "ask"; readonly reason: string }
  | { readonly kind: "refused"; readonly reason: string; readonly unreached: boolean };

/**
 * The one check of a Deploy a person may approve, at its link and again at its decision, from the Store's fresh `plan`,
 * the tap read for it (`servers`, and `source` for the rule) and the `rule` it carries. `covered` are the Services it
 * already carries a version of. It checks only what a tap signs: whether each Server takes a build is the Server's to
 * say, and the Deployment shows its refusal.
 */
export function admission(input: {
  readonly plan: Pick<PlanView, "unsigned" | "sources">;
  readonly servers: DeployTap;
  readonly source: DeploySource | undefined;
  readonly covered: ReadonlySet<string | null>;
  readonly rule: CarriedRule;
}): Admission {
  const { plan, servers, source, covered, rule } = input;
  const open = (plan.unsigned ?? []).filter((entry) => !covered.has(entry.service) && !underRule(entry));
  // A rule swapped or changed after the tap would be kept, so it is refused.
  if (rule.kind === "signed" && rule.sourced === null) {
    return { kind: "refused", reason: "This deploy's automatic deploy rule isn't the one your passkey signed. Approve it again.", unreached: false };
  }
  if (servers.kind === "refused" && open.length > 0) return { kind: "refused", reason: servers.message, unreached: true };
  if (servers.kind !== "ready") return { kind: "admit", source: false };
  const stops = blocked(open);
  if (stops !== null) return { kind: "refused", reason: stops, unreached: false };
  if (open.length > 0) return { kind: "ask", reason: "Sign this deploy with your passkey on its approval page." };
  return { kind: "admit", source: rule.kind === "none" && source?.kind === "sign" && (plan.sources ?? []).length > 0 };
}

/** Why a Service no passkey can sign keeps the Deploy from asking, or null when a passkey can sign each of `unsigned`. */
export function blocked(unsigned: readonly Unsigned[]) {
  for (const { service, needs, ended } of unsigned) {
    if (ended === true) return `Automatic deploys are off for ${service}. Turn on Deploy every push to deploy it.`;
    if (needs === "digest") return `${service} pulls a private tag. Write its image by digest (image@sha256:…).`;
  }
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
  const unsigned = (plan.unsigned ?? []).filter((entry) => !covered.has(entry.service) && !underRule(entry));
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
export const withAnswer = (command: AdmitDeploy, answer: Answer): AdmitDeploy =>
  answer.rule === undefined ? { ...command, versions: answer.versions } : { ...command, versions: answer.versions, rule: answer.rule };

/** An answer to plan `plan` in Environment `place` whose Deploy the Store asked to confirm: the confirmed retry carries it. */
export type Answered = { readonly plan: string; readonly place: string; readonly answer: Answer; readonly held: Held | null };

/** How a Deploy is sent, and where its answer waits for a confirmed retry. */
export type DeploySteps = {
  readonly answered: { current: Answered | null };
  readonly commit: (command: AdmitDeploy, handles: readonly string[]) => Promise<void>;
  readonly prepare: (command: AdmitDeploy) => Promise<Prepared>;
  /** Asks the person to sign `ask`; settles once their Deploy was sent, or throws why it wasn't. */
  readonly ask: (ask: Ask, handles: readonly string[]) => Promise<void>;
  /** Tells the person their changes moved; the Deploy then throws. */
  readonly stale: (command: AdmitDeploy) => Promise<Error>;
  /** Tells the person why nothing deployed; the Deploy then throws. */
  readonly refused: (message: string) => Error;
  /** Tells the person the Deploy went without the rule, and why. */
  readonly held: (held: Held | null) => void;
};

/** Sends `command` with `answered`'s answer, keeping it when the Store asks to confirm with one of `handles`. */
export async function sendAnswer(steps: Pick<DeploySteps, "answered" | "commit" | "held">, command: AdmitDeploy, answered: Answered, handles: readonly string[]) {
  try {
    await steps.commit(withAnswer(command, answered.answer), handles);
  } catch (error) {
    // The Store asks again (Servers hold more by now): the caller's next accepted retry still needs no tap.
    if (error instanceof StoreRefused && handles.includes(error.code)) steps.answered.current = answered;
    throw error;
  }
  steps.held(answered.held);
}

/**
 * Deploys `command`. A confirmed retry of the review an answer was signed for carries that answer, the rule and the
 * rule it read as kept included, so the Store refuses the whole Deploy if the kept rule moved since, rather than
 * deploying without it.
 */
export async function deployWith(steps: DeploySteps, command: AdmitDeploy, handles: readonly string[] = []): Promise<void> {
  const answered = steps.answered.current;
  steps.answered.current = null;
  if (answered !== null && answered.place === environmentKey(command.environment)
    && command.version !== null && command.version !== undefined && namesReview(answered.plan, command.version)) {
    return sendAnswer(steps, command, answered, handles);
  }
  const prepared = await steps.prepare(command);
  if (prepared.kind === "stale") throw await steps.stale(command);
  if (prepared.kind === "refused") throw steps.refused(prepared.message);
  if (prepared.kind === "ask") return steps.ask(prepared.ask, handles);
  await steps.commit(prepared.command, handles);
  steps.held(prepared.held);
}
