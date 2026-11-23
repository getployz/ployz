import "@tanstack/react-start/server-only";
import { pinImages, type EnvironmentRef, type PlanView } from "@ployz/sdk";
import { and, eq } from "drizzle-orm";
import { Effect } from "effect";
import { callStore } from "#/modules/config-store/config-store.server";
import type { Actor } from "#/modules/identity/actor";
import { requireAccess } from "#/modules/organization/access.server";
import { answerForSupport } from "#/modules/support-access/support-view";
import { trustedPasskeys } from "#/modules/passkeys/passkeys.server";
import { passkeyRequest } from "#/modules/passkeys/tables";
import { codeOfPasskeyKey } from "#/modules/passkeys/passkey-key";
import { countOrganizationMachines } from "#/modules/runtime/connections.server";
import { firstRuntimeFrame } from "#/modules/runtime/organization-runtime.server";
import { Database } from "#/server/database.server";

/** What a Server that enforces device approval needs to check a passkey's versions. */
const SIGNS = ["ployz.container.signed-version.proposal.v1", "ployz.authority.scoped-keys.v1"];

const UNREACHED = "Couldn't reach your servers. Try again.";

/**
 * What a person's Deploy needs before it asks for their passkey: nothing when no Server enforces device approval, else
 * the Cluster's key, the person's passkeys every enforcing Server trusts, and the digest each tag the plan signs runs
 * as. Any other answer is why it can't ask, in words the sheet shows.
 */
export type DeployTap =
  | { readonly kind: "off" }
  | { readonly kind: "refused"; readonly message: string }
  | {
    readonly kind: "ready";
    readonly cluster: string;
    readonly passkeys: readonly { readonly credentialId: string; readonly code: string }[];
    readonly pins: Readonly<Record<string, string>>;
  };

/** The plan Cloud read for a Deploy, with what asking for it needs, or the Store's refusal of the read. */
export type DeployPlan =
  | { readonly kind: "plan"; readonly plan: PlanView; readonly tap: DeployTap }
  | { readonly kind: "refused"; readonly message: string };

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
  if (!read.ok) return { kind: "refused", message: read.refusal.message } satisfies DeployPlan;
  const plan = read.value;
  const unsigned = plan.unsigned ?? [];
  const signs = new Set(unsigned.filter((entry) => entry.needs === "version").map((entry) => entry.service));
  // A Service only a rule or a digest can cover refuses before any lookup, unless nothing enforces.
  const tags = signs.size < unsigned.length ? [] : (plan.target ?? []).filter((spec) => signs.has(spec.name))
    .map((spec) => spec.container.image).filter((image) => !image.includes("@sha256:"));
  const tap: DeployTap = unsigned.length === 0 ? { kind: "off" } : yield* gate(actor.userId, access.organization.id, tags);
  // Support never signs, and reads a plan only as its own view of it allows.
  if (access.kind === "support") {
    if (tap.kind !== "off") return { kind: "refused", message: "Support can't approve this Deploy. Ask a member to deploy it." } satisfies DeployPlan;
    const shown = answerForSupport({ operation: "read", query: { query: "plan", environment: input.environment, services: [...input.services], target: true } }, read, access.shows);
    if (!shown.ok) return { kind: "refused", message: shown.refusal.message } satisfies DeployPlan;
    return { kind: "plan", plan: shown.value, tap } satisfies DeployPlan;
  }
  return { kind: "plan", plan, tap } satisfies DeployPlan;
});

const gate = Effect.fn("DeployTap.gate")(function* (userId: string, organizationId: string, tags: readonly string[]) {
  const refused = (message: string): DeployTap => ({ kind: "refused", message });
  // With no Server paired nothing enforces; the first one deploys what is saved.
  if ((yield* countOrganizationMachines(organizationId)) === 0) return { kind: "off" } satisfies DeployTap;
  const frame = yield* firstRuntimeFrame(organizationId).pipe(Effect.orElseSucceed(() => null));
  // A Server the frame couldn't read may enforce: its answer is unknown, not off.
  if (frame === null || frame.incomplete_ids.machines.length > 0) return refused(UNREACHED);
  const enforcing = frame.machines.map(({ machine }) => machine).filter((machine) => machine.runtime.device_enforcement === true);
  if (enforcing.length === 0) return { kind: "off" } satisfies DeployTap;
  const older = enforcing.find(({ runtime: { capabilities = [] } }) => SIGNS.some((needed) => !capabilities.includes(needed)));
  if (older !== undefined) return refused(`Upgrade ${older.name} first. Its ployz can't check your passkey.`);
  const cluster = enforcing.flatMap(({ runtime }) => runtime.sealing?.kind === "held" ? [runtime.sealing.public_key] : [])[0];
  if (cluster === undefined) return refused("Your servers didn't report the cluster's key. Try again.");
  const answers = (yield* trustedPasskeys(organizationId)) ?? [];
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
  const passkeys = own.filter((row) => lists.every((held) => held?.some(({ trusted }) => codeOfPasskeyKey(trusted.passkey.public_key) === row.code)));
  if (passkeys.length === 0) return refused("Your servers don't trust your passkey. Add it under Account, Passkeys, or run ployz deploy.");
  return yield* Effect.tryPromise(() => pinImages([...new Set(tags)])).pipe(
    Effect.map((images): DeployTap => ({ kind: "ready", cluster, passkeys, pins: Object.fromEntries(images.map(({ tag, image }) => [tag, image])) })),
    Effect.catch((error) => Effect.succeed(refused(error.cause instanceof Error ? error.cause.message : "Couldn't look up an image's digest. Try again."))),
  );
});
