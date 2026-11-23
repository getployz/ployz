/**
 * A person's Deploy from the browser, signed with one passkey prompt when its Servers need it. The Store's plan names
 * each Service a Server would run only signed (`unsigned`). The browser drafts one version of each with the ployz-core
 * code the Servers check, shows the lines it signs, and the passkey answers one bundle of them. Cloud only relays the
 * plan, the tag pins and the answer, and each Server checks every step.
 */
import type { Needs, PostedVersion, VersionsDraft } from "@ployz/sdk";
import { Option, Schema } from "effect";
import { versionSteps } from "@ployz/sdk/authority";
import type { AdmitDeploy } from "#/modules/config-store/admit-deploy";
import { type OnlyPasskey, signWithPasskey } from "#/modules/passkeys/passkey-statement";
import { LOSS_MARK } from "#/modules/volume-placement/volume-placement.review";
import type { DeployPlan } from "./deploy-tap.server";

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

/** Why a Service no passkey can sign keeps the Deploy from asking. */
export const BLOCKED = {
  rule: (service) => `No automatic deploy rule covers ${service}. Approve one, or run ployz deploy.`,
  rebuild: (service) => `Push to ${service}'s branch first. Its rule hasn't built this version yet.`,
  digest: (service) => `${service} pulls a private tag. Write its image by digest (image@sha256:…).`,
} satisfies Record<Exclude<Needs, "version">, (service: string) => string>;

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
};

export type Prepared =
  | { readonly kind: "admit"; readonly command: AdmitDeploy }
  | { readonly kind: "stale" }
  | { readonly kind: "refused"; readonly message: string }
  | { readonly kind: "ask"; readonly ask: Ask };

/**
 * What `command` needs before it is admitted: nothing, a passkey's answer to the drafted versions, or a reason it can't
 * ask. Nothing prompts here, and a reason is given before any prompt. Every exit admits at the caller's version as
 * given, `#loss-` mark and digest included, or at the plan's when the caller named none.
 */
export async function prepareDeploy(
  command: AdmitDeploy,
  read: (command: AdmitDeploy) => Promise<DeployPlan>,
  now = Date.now(),
): Promise<Prepared> {
  const planned = await read(command);
  if (planned.kind === "refused") return planned;
  const { plan, tap: gate } = planned;
  if (command.version !== null && command.version !== undefined && !namesReview(plan.version, command.version)) return { kind: "stale" };
  const bound: AdmitDeploy = { ...command, version: command.version ?? plan.version };
  // A version the Deploy already carries, such as one a CLI's device key signed, covers its Service: only the rest ask.
  const covered = new Set((command.versions ?? []).map(versionService));
  const unsigned = (plan.unsigned ?? []).filter((entry) => !covered.has(entry.service));
  if (gate.kind === "off" || unsigned.length === 0) return { kind: "admit", command: bound };
  if (gate.kind === "refused") return gate;
  const blocked = unsigned.find((entry) => entry.needs !== "version");
  if (blocked && blocked.needs !== "version") return { kind: "refused", message: BLOCKED[blocked.needs](blocked.service) };
  const names = new Set(unsigned.map((entry) => entry.service));
  const specs = (plan.target ?? []).filter((spec) => names.has(spec.name));
  let draft: VersionsDraft;
  try {
    draft = versionSteps({
      cluster: gate.cluster, namespace: plan.namespace, volumes: plan.volumes, services: specs, pins: gate.pins,
      operation: crypto.randomUUID().replaceAll("-", ""), issued_at: now, expires_at: now + WINDOW_MS,
      seed: [...crypto.getRandomValues(new Uint8Array(16))], against: gate.against,
    });
  } catch (error) {
    return { kind: "refused", message: error instanceof Error ? error.message : "This browser couldn't draft the deploy." };
  }
  return {
    kind: "ask",
    ask: { namespace: plan.namespace, services: draft.steps.map((step) => step.service), draft, passkeys: gate.passkeys, command: bound, plan: plan.version },
  };
}

/** `ask`'s versions, each a step of the one bundle the person's passkey answers, as the Deploy posts them. */
export async function signVersions(ask: Ask): Promise<PostedVersion[]> {
  const proposal = await signWithPasskey(ask.draft.bundle, "workflow", ask.passkeys);
  return ask.draft.steps.map((step) => {
    const posted: PostedVersion = { statement: { body: step.body, signatures: [], proposal }, births: step.births };
    if (step.tag !== undefined) posted.pin = { tag: step.tag, image: step.image };
    return posted;
  });
}
