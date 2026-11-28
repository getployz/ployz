import "@tanstack/react-start/server-only";
import type { EnvironmentRef, JsonValue, Parent, PlanView, RuleView } from "@ployz/sdk";
import { type Against, sourceNeed } from "@ployz/sdk/authority";
import { and, eq } from "drizzle-orm";
import { Effect } from "effect";
import { callStore } from "#/modules/config-store/config-store.server";
import type { Actor } from "#/modules/identity/actor";
import { beyondEnvironments, requireAccess } from "#/modules/organization/access.server";
import type { Permission } from "#/modules/organization/permissions";
import { environmentProtected } from "#/modules/config-store/store-lookup.server";
import { answerForSupport } from "#/modules/support-access/support-view";
import { trustedPasskeys } from "#/modules/passkeys/passkeys.server";
import { passkeyRequest } from "#/modules/passkeys/tables";
import { codeOfPasskeyKey } from "#/modules/passkeys/passkey-key";
import { listOrganizationServers } from "#/modules/runtime/connections.server";
import { firstRuntimeFrame, OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import { pinImageDigests } from "#/modules/runtime/ployz.server";
import { underRule } from "#/modules/deploy-approval/deploy-tap";
import { Database } from "#/server/database.server";

/** What a Server needs to check a passkey's versions. */
const SIGNS = ["ployz.container.signed-version.proposal.v1", "ployz.authority.scoped-keys.v1"];

/** What it also needs to take an automatic deploy rule a passkey signed as a step of a bundle. */
const SIGNS_RULE = [...SIGNS, "ployz.container.deploy-rule.v1"];

const UNREACHED = "Couldn't reach your servers. Try again.";

/**
 * What a person's Deploy needs before it asks for their passkey: nothing when no Server is paired yet or the person
 * only asks for it, else the Cluster's key, the person's passkeys every Server trusts, the digest each
 * tag the plan signs runs as, and what each mount is compared with. Any other answer is why it can't ask, in words the
 * sheet shows.
 */
export type DeployTap =
  | { readonly kind: "off" }
  | { readonly kind: "refused"; readonly message: string }
  | {
    readonly kind: "ready";
    readonly cluster: string;
    readonly passkeys: readonly { readonly credentialId: string; readonly code: string }[];
    readonly pins: Readonly<Record<string, string>>;
    readonly against: Against;
    /** Every Server of the Cluster, by ID, which a rule names. */
    readonly members: readonly string[];
  };

/**
 * The automatic deploy rule a Deploy may sign with its tap when the kept rule doesn't approve what it saves: the staged
 * rule view to draft it from, or why the tap can't sign it, so the Deploy goes without it and pushes wait. `unchecked`
 * marks a rule only the Servers it names may leave behind, or one Cloud couldn't check: a push may not wait.
 */
export type DeploySource =
  | { readonly kind: "sign"; readonly view: RuleView; readonly unchecked: boolean }
  | { readonly kind: "held"; readonly reason: string; readonly unchecked: boolean };

/** The plan Cloud read for a Deploy, with what asking for it needs, or the Store's refusal of the read. */
export type DeployPlan =
  | { readonly kind: "plan"; readonly plan: PlanView; readonly tap: DeployTap; readonly source?: DeploySource }
  | { readonly kind: "refused"; readonly message: string; readonly details?: JsonValue };

/**
 * The plan of a Deploy of `services` in `environment` with its targets, read here so Cloud looks up only the tags of
 * the specs it read itself, never ones the browser names.
 */
type TapInput = {
  readonly organizationSlug: string;
  readonly environment: EnvironmentRef;
  readonly services: readonly string[];
  /** Classify a Deploy someone else will sign: the gate checks the Servers, not `actor`'s passkeys. */
  readonly classify?: true;
  /** Read whether the Deploy's own tap signs the automatic deploy rule with it. */
  readonly rule?: boolean;
};

export const prepareDeployTap = Effect.fn("DeployTap.prepare")(function* (actor: Actor, input: TapInput) {
  const access = yield* requireAccess(actor, input.organizationSlug, "changes:deploy");
  const read = yield* callStore(access.organization.id, actor.userId, {
    operation: "read",
    query: { query: "plan", environment: input.environment, services: [...input.services], target: true },
  }, { approval: "not_required", permissions: access.permissions, environments: access.environments });
  if (!read.ok) return { kind: "refused", message: read.refusal.message, details: read.refusal.details } satisfies DeployPlan;
  const plan = read.value;
  const unsigned = (plan.unsigned ?? []).filter((entry) => !underRule(entry));
  const signs = new Set(unsigned.filter((entry) => entry.needs === "version").map((entry) => entry.service));
  // A Service only a digest or Deploy every push can cover refuses before any lookup, unless nothing enforces.
  const tags = signs.size < unsigned.length ? [] : (plan.target ?? []).filter((spec) => signs.has(spec.name))
    .map((spec) => spec.container.image).filter((image) => !image.includes("@sha256:"));
  // The Store keeps each Service's last approved version with its Sealed values, so support never reads it. A Store that
  // can't say leaves each Service with nothing to compare with, so every mount of existing data asks.
  const parents = signs.size === 0 || access.kind === "support" ? Effect.succeed({}) : callStore(access.organization.id, actor.userId, {
    operation: "read", query: { query: "parent", environment: input.environment, services: [...signs] },
  }, { approval: "not_required", permissions: access.permissions, environments: access.environments })
    .pipe(Effect.map((read): Readonly<Record<string, Parent>> => read.ok ? read.value.services : {}));
  // A member who can't protect this Protected Environment signs nothing here: their Deploy becomes a proposal, and the
  // owner or admin who approves it signs it there.
  const asks = input.classify !== true && access.kind === "member" && unsigned.length > 0 && !mayProtect(access, plan.environment.name)
    && (yield* environmentProtected(access.organization.id, plan.environment.project, plan.environment.id).pipe(
      Effect.catchTag("StoreRefused", () => Effect.succeed(false)),
    ));
  const signer = input.classify === true ? null : actor.userId;
  // Only a member who signs here may sign the rule with the Deploy. Classified, it says whether whoever approves it signs one.
  let source = input.rule !== true || access.kind !== "member" ? null : yield* sourceOf(access, input.environment, plan.version, actor.userId, signer === null);
  const versions = unsigned.length === 0 || asks ? Effect.succeed<DeployTap>({ kind: "off" })
    : gate(signer, access.organization.id, tags, { namespace: plan.namespace, parents, needs: SIGNS });
  let tap: DeployTap;
  if (source?.kind === "sign") {
    const ruled = yield* gate(signer, access.organization.id, tags, { namespace: plan.namespace, parents, needs: SIGNS_RULE });
    // A Deploy that can't sign the rule still deploys what it can.
    if (ruled.kind === "refused") source = { kind: "held", reason: ruled.message, unchecked: source.unchecked };
    if (ruled.kind === "off") source = null;
    tap = ruled.kind === "refused" ? yield* versions : ruled;
  } else {
    tap = yield* versions;
  }
  // Support never signs, and reads a plan only as its own view of it allows.
  if (access.kind === "support") {
    if (tap.kind !== "off") return { kind: "refused", message: "Support can't approve this Deploy. Ask a member to deploy it." } satisfies DeployPlan;
    const shown = answerForSupport({ operation: "read", query: { query: "plan", environment: input.environment, services: [...input.services], target: true } }, read, access.shows);
    if (!shown.ok) return { kind: "refused", message: shown.refusal.message } satisfies DeployPlan;
    return { kind: "plan", plan: shown.value, tap } satisfies DeployPlan;
  }
  const planned: DeployPlan = { kind: "plan", plan, tap };
  return source === null ? planned : { ...planned, source };
});

/**
 * What a Deploy at review `version` of `environment` needs for its Source step, read from the staged rule view alone,
 * before any Server or passkey is reached: null when it needs none, when nothing builds from GitHub, when the review
 * moved, or when a protector decides the member's writes here, unless `classify`: someone else approves it then.
 */
const sourceOf = Effect.fn("DeployTap.source")(function* (access: Access, environment: EnvironmentRef, version: string, userId: string, classify: boolean) {
  const read = yield* callStore(access.organization.id, userId, {
    operation: "read", query: { query: "rule", environment, staged: true },
  }, { approval: "not_required", permissions: access.permissions, environments: access.environments });
  const unread: DeploySource = { kind: "held", reason: "Couldn't check automatic deploys.", unchecked: true };
  if (!read.ok) return unread;
  const view = read.value;
  // Read with the view: a protection Cloud couldn't read never reads as none.
  if (view.sources.length === 0 || view.version !== version || (!classify && view.protected !== false && !mayProtect(access, view.environment.name))) return null;
  let need: ReturnType<typeof sourceNeed>;
  try {
    need = sourceNeed(view);
  } catch {
    return unread;
  }
  if (need === "none") return null;
  return { kind: "sign", view, unchecked: need === "servers" } satisfies DeploySource;
});

type Access = {
  readonly organization: { readonly id: string };
  readonly permissions: Permission[];
  readonly environments: string[] | undefined;
};

/** What a Save's tap reads: the rule view of what the Save saves and, once asked, what asking for a passkey needs. */
export type SaveTap =
  | { readonly kind: "off" }
  | { readonly kind: "refused"; readonly message: string }
  | { readonly kind: "view"; readonly view: RuleView; readonly tap: DeployTap | null }
  /** A member who can stage but not deploy: their Save keeps no rule, so the view only says whether it holds pushes. */
  | { readonly kind: "unsigned"; readonly view: RuleView };

/**
 * What a person's Save of `environment` reads before it can sign the automatic deploy rule for what it saves: nothing
 * when they can't deploy here or a protector decides their writes here, else the staged rule view and, with `gate`,
 * what the Deploy tap reads for a passkey. Only a failed staged read refuses.
 */
export const prepareSaveTap = Effect.fn("SaveTap.prepare")(function* (actor: Actor, input: {
  readonly organizationSlug: string;
  readonly environment: EnvironmentRef;
  readonly gate: boolean;
}) {
  const access = yield* requireAccess(actor, input.organizationSlug, "changes:stage");
  if (access.kind !== "member") return { kind: "off" } satisfies SaveTap;
  // The Store keeps a Save's rule only for someone who may deploy.
  const deploys = access.permissions.includes("changes:deploy");
  const read = yield* callStore(access.organization.id, actor.userId, {
    operation: "read", query: { query: "rule", environment: input.environment, staged: true },
  }, { approval: "not_required", permissions: access.permissions, environments: access.environments });
  // Only a Save that could carry a rule waits on the read.
  if (!read.ok) return deploys ? { kind: "refused", message: read.refusal.message } satisfies SaveTap : { kind: "off" } satisfies SaveTap;
  const view = read.value;
  // Read with the view: a protection Cloud couldn't read never reads as none.
  if (view.sources.length === 0 || (view.protected !== false && !mayProtect(access, view.environment.name))) {
    return { kind: "off" } satisfies SaveTap;
  }
  if (!deploys) return { kind: "unsigned", view } satisfies SaveTap;
  const tap = input.gate ? yield* gate(actor.userId, access.organization.id, [], { namespace: view.namespace, parents: Effect.succeed({}), needs: SIGNS_RULE }) : null;
  return { kind: "view", view, tap } satisfies SaveTap;
});

const mayProtect = (access: Omit<Access, "organization">, environment: string) =>
  access.permissions.includes("environments:protect") && beyondEnvironments(access.environments, { environment }) === null;

/** `userId` null only classifies a Deploy someone else signs: no one's passkeys, and no digest lookups. */
export const gate = Effect.fn("DeployTap.gate")(function* <E, R>(userId: string | null, organizationId: string, tags: readonly string[], asked: {
  readonly namespace: string;
  /** Read only once the gate would ask. */
  readonly parents: Effect.Effect<Readonly<Record<string, Parent>>, E, R>;
  /** The capabilities each Server needs to check what is signed. */
  readonly needs: readonly string[];
}) {
  const refused = (message: string): DeployTap => ({ kind: "refused", message });
  // With no Server paired nothing checks; the first one deploys what is saved.
  const paired = yield* listOrganizationServers(organizationId);
  if (paired.length === 0) return { kind: "off" } satisfies DeployTap;
  const frame = yield* firstRuntimeFrame(organizationId).pipe(Effect.orElseSucceed(() => null));
  // A Server the frame couldn't read is unknown: its key and passkeys are unread.
  if (frame === null || frame.incomplete_ids.machines.length > 0) return refused(UNREACHED);
  // A paired Server the frame doesn't list is named rather than skipped.
  const seen = new Set(frame.machines.map(({ machine }) => machine.id));
  const unseen = paired.find(({ id }) => !seen.has(id));
  if (unseen !== undefined) return refused(`Couldn't reach ${unseen.name ?? unseen.id}. Try again.`);
  const servers = frame.machines.map(({ machine }) => machine);
  // A Server that says it doesn't enforce runs Ployz 0.3, which refuses every signed version.
  const older = servers.find(({ runtime: { capabilities = [], device_enforcement } }) =>
    device_enforcement === false || asked.needs.some((needed) => !capabilities.includes(needed)));
  if (older !== undefined) return refused(`Upgrade ${older.name} first. Its ployz can't check your passkey.`);
  const keyed = servers.flatMap(({ id, runtime }) => runtime.sealing?.kind === "held" ? [{ id, cluster: runtime.sealing.public_key }] : [])[0];
  if (keyed === undefined) return refused("Your servers didn't report the cluster's key. Try again.");
  const { cluster } = keyed;
  const [found, listed] = yield* Effect.all([trustedPasskeys(organizationId), listContainers(organizationId, asked.namespace)], { concurrency: 2 });
  const answers = found ?? [];
  if (answers.length === 0) return refused(UNREACHED);
  const lists: (typeof answers)[number]["passkeys"][] = [];
  for (const server of servers) {
    const held = answers.find(({ id }) => id === server.id)?.passkeys;
    if (held === null || held === undefined) return refused(`Couldn't reach ${server.name}. Try again.`);
    lists.push(held);
  }
  const { drizzle } = yield* Database;
  const own = userId === null ? null : yield* drizzle.select({ code: passkeyRequest.code, credentialId: passkeyRequest.credentialId }).from(passkeyRequest)
    .where(and(eq(passkeyRequest.organizationId, organizationId), eq(passkeyRequest.userId, userId)));
  const trusted = answers.find(({ id }) => id === keyed.id)?.keys;
  if (trusted === null || trusted === undefined) return refused(UNREACHED);
  const passkeys = (own ?? []).filter((row) => lists.every((held) => held?.some(({ trusted }) => codeOfPasskeyKey(trusted.passkey.public_key) === row.code)));
  if (own !== null && passkeys.length === 0) return refused("Your servers don't trust your passkey. Add it under Account, Passkeys, or run ployz deploy.");
  // Every paired Server, and any the frame, the trust read or the listing knew of, by name, that didn't list its own
  // Containers. A later source's name wins; a paired Server no source named reads as its ID.
  const answered = new Set(listed?.answered);
  const roster = new Map([
    ...paired.map(({ id, name }) => [id, name ?? id] as const),
    ...(listed?.unanswered ?? []).map(({ machine, name }) => [machine, name] as const),
    ...answers.flatMap(({ id, server }) => id === null ? [] : [[id, server] as const]),
    ...frame.machines.map(({ machine }) => [machine.id, machine.name] as const),
  ]);
  const against: Against = {
    parents: yield* asked.parents, trusted, containers: listed?.containers ?? [],
    unanswered: [...roster].filter(([id]) => !answered.has(id)).map(([, name]) => name),
  };
  const members = frame.machines.filter(({ machine }) => machine.runtime.sealing?.kind === "held" && machine.runtime.sealing.public_key === cluster)
    .map(({ machine }) => machine.id).sort();
  if (userId === null) return { kind: "ready", cluster, passkeys, pins: {}, against, members } satisfies DeployTap;
  return yield* pinImageDigests([...new Set(tags)]).pipe(
    Effect.map((images): DeployTap => ({ kind: "ready", cluster, passkeys, pins: Object.fromEntries(images.map(({ tag, image }) => [tag, image])), against, members })),
    Effect.catch((error) => Effect.succeed(refused(error.cause instanceof Error ? error.cause.message : "Couldn't look up an image's digest. Try again."))),
  );
});

/** The Containers of `namespace` that each Server lists itself, or null when Cloud reached none of them. */
const listContainers = (organizationId: string, namespace: string) => Effect.gen(function* () {
  const session = yield* (yield* OrganizationRuntime).open(organizationId);
  return session.status === "connected" ? yield* session.connected.observeNamespace(namespace) : null;
}).pipe(Effect.scoped, Effect.orElseSucceed(() => null));
