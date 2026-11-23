import "@tanstack/react-start/server-only";
import type { EnvironmentRef, JsonValue, Parent, PlanView } from "@ployz/sdk";
import type { Against } from "@ployz/sdk/authority";
import { and, eq } from "drizzle-orm";
import { Effect } from "effect";
import { callStore } from "#/modules/config-store/config-store.server";
import type { Actor } from "#/modules/identity/actor";
import { requireAccess } from "#/modules/organization/access.server";
import { answerForSupport } from "#/modules/support-access/support-view";
import { trustedPasskeys } from "#/modules/passkeys/passkeys.server";
import { passkeyRequest } from "#/modules/passkeys/tables";
import { codeOfPasskeyKey } from "#/modules/passkeys/passkey-key";
import { listOrganizationServers } from "#/modules/runtime/connections.server";
import { firstRuntimeFrame, OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import { pinImageDigests } from "#/modules/runtime/ployz.server";
import { Database } from "#/server/database.server";

/** What a Server that enforces device approval needs to check a passkey's versions. */
const SIGNS = ["ployz.container.signed-version.proposal.v1", "ployz.authority.scoped-keys.v1"];

const UNREACHED = "Couldn't reach your servers. Try again.";

/**
 * What a person's Deploy needs before it asks for their passkey: nothing when no Server enforces device approval, else
 * the Cluster's key, the person's passkeys every enforcing Server trusts, the digest each tag the plan signs runs as,
 * and what each mount is compared with. Any other answer is why it can't ask, in words the sheet shows.
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
  };

/** The plan Cloud read for a Deploy, with what asking for it needs, or the Store's refusal of the read. */
export type DeployPlan =
  | { readonly kind: "plan"; readonly plan: PlanView; readonly tap: DeployTap }
  | { readonly kind: "refused"; readonly message: string; readonly details?: JsonValue };

/**
 * The plan of a Deploy of `services` in `environment` with its targets, read here so Cloud looks up only the tags of
 * the specs it read itself, never ones the browser names.
 */
export const prepareDeployTap = Effect.fn("DeployTap.prepare")(function* (actor: Actor, input: {
  readonly organizationSlug: string;
  readonly environment: EnvironmentRef;
  readonly services: readonly string[];
}) {
  const access = yield* requireAccess(actor, input.organizationSlug, "changes:deploy");
  const read = yield* callStore(access.organization.id, actor.userId, {
    operation: "read",
    query: { query: "plan", environment: input.environment, services: [...input.services], target: true },
  }, { approval: "not_required", permissions: access.permissions, environments: access.environments });
  if (!read.ok) return { kind: "refused", message: read.refusal.message, details: read.refusal.details } satisfies DeployPlan;
  const plan = read.value;
  const unsigned = plan.unsigned ?? [];
  const signs = new Set(unsigned.filter((entry) => entry.needs === "version").map((entry) => entry.service));
  // A Service only a rule or a digest can cover refuses before any lookup, unless nothing enforces.
  const tags = signs.size < unsigned.length ? [] : (plan.target ?? []).filter((spec) => signs.has(spec.name))
    .map((spec) => spec.container.image).filter((image) => !image.includes("@sha256:"));
  // The Store keeps each Service's last approved version with its Sealed values, so support never reads it. A Store that
  // can't say leaves each Service with nothing to compare with, so every mount of existing data asks.
  const parents = signs.size === 0 || access.kind === "support" ? Effect.succeed({}) : callStore(access.organization.id, actor.userId, {
    operation: "read", query: { query: "parent", environment: input.environment, services: [...signs] },
  }, { approval: "not_required", permissions: access.permissions, environments: access.environments })
    .pipe(Effect.map((read): Readonly<Record<string, Parent>> => read.ok ? read.value.services : {}));
  const tap: DeployTap = unsigned.length === 0 ? { kind: "off" } : yield* gate(actor.userId, access.organization.id, tags, { namespace: plan.namespace, parents });
  // Support never signs, and reads a plan only as its own view of it allows.
  if (access.kind === "support") {
    if (tap.kind !== "off") return { kind: "refused", message: "Support can't approve this Deploy. Ask a member to deploy it." } satisfies DeployPlan;
    const shown = answerForSupport({ operation: "read", query: { query: "plan", environment: input.environment, services: [...input.services], target: true } }, read, access.shows);
    if (!shown.ok) return { kind: "refused", message: shown.refusal.message } satisfies DeployPlan;
    return { kind: "plan", plan: shown.value, tap } satisfies DeployPlan;
  }
  return { kind: "plan", plan, tap } satisfies DeployPlan;
});

const gate = Effect.fn("DeployTap.gate")(function* <E, R>(userId: string, organizationId: string, tags: readonly string[], asked: {
  readonly namespace: string;
  /** Read only once the gate would ask. */
  readonly parents: Effect.Effect<Readonly<Record<string, Parent>>, E, R>;
}) {
  const refused = (message: string): DeployTap => ({ kind: "refused", message });
  // With no Server paired nothing enforces; the first one deploys what is saved.
  const paired = yield* listOrganizationServers(organizationId);
  if (paired.length === 0) return { kind: "off" } satisfies DeployTap;
  const frame = yield* firstRuntimeFrame(organizationId).pipe(Effect.orElseSucceed(() => null));
  // A Server the frame couldn't read may enforce: its answer is unknown, not off.
  if (frame === null || frame.incomplete_ids.machines.length > 0) return refused(UNREACHED);
  // A paired Server the frame doesn't list may enforce: name it rather than read its silence as off.
  const seen = new Set(frame.machines.map(({ machine }) => machine.id));
  const unseen = paired.find(({ id }) => !seen.has(id));
  if (unseen !== undefined) return refused(`Couldn't reach ${unseen.name ?? unseen.id}. Try again.`);
  const enforcing = frame.machines.map(({ machine }) => machine).filter((machine) => machine.runtime.device_enforcement === true);
  if (enforcing.length === 0) return { kind: "off" } satisfies DeployTap;
  const older = enforcing.find(({ runtime: { capabilities = [] } }) => SIGNS.some((needed) => !capabilities.includes(needed)));
  if (older !== undefined) return refused(`Upgrade ${older.name} first. Its ployz can't check your passkey.`);
  const keyed = enforcing.flatMap(({ id, runtime }) => runtime.sealing?.kind === "held" ? [{ id, cluster: runtime.sealing.public_key }] : [])[0];
  if (keyed === undefined) return refused("Your servers didn't report the cluster's key. Try again.");
  const { cluster } = keyed;
  const [found, listed] = yield* Effect.all([trustedPasskeys(organizationId), listContainers(organizationId, asked.namespace)], { concurrency: 2 });
  const answers = found ?? [];
  if (answers.length === 0) return refused(UNREACHED);
  const lists: (typeof answers)[number]["passkeys"][] = [];
  for (const server of enforcing) {
    const held = answers.find(({ id }) => id === server.id)?.passkeys;
    if (held === null || held === undefined) return refused(`Couldn't reach ${server.name}. Try again.`);
    lists.push(held);
  }
  const { drizzle } = yield* Database;
  const own = yield* drizzle.select({ code: passkeyRequest.code, credentialId: passkeyRequest.credentialId }).from(passkeyRequest)
    .where(and(eq(passkeyRequest.organizationId, organizationId), eq(passkeyRequest.userId, userId)));
  const trusted = answers.find(({ id }) => id === keyed.id)?.keys;
  if (trusted === null || trusted === undefined) return refused(UNREACHED);
  const passkeys = own.filter((row) => lists.every((held) => held?.some(({ trusted }) => codeOfPasskeyKey(trusted.passkey.public_key) === row.code)));
  if (passkeys.length === 0) return refused("Your servers don't trust your passkey. Add it under Account, Passkeys, or run ployz deploy.");
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
  return yield* pinImageDigests([...new Set(tags)]).pipe(
    Effect.map((images): DeployTap => ({ kind: "ready", cluster, passkeys, pins: Object.fromEntries(images.map(({ tag, image }) => [tag, image])), against })),
    Effect.catch((error) => Effect.succeed(refused(error.cause instanceof Error ? error.cause.message : "Couldn't look up an image's digest. Try again."))),
  );
});

/** The Containers of `namespace` that each Server lists itself, or null when Cloud reached none of them. */
const listContainers = (organizationId: string, namespace: string) => Effect.gen(function* () {
  const session = yield* (yield* OrganizationRuntime).open(organizationId);
  return session.status === "connected" ? yield* session.connected.observeNamespace(namespace) : null;
}).pipe(Effect.scoped, Effect.orElseSucceed(() => null));
