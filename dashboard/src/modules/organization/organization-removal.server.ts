import "@tanstack/react-start/server-only";
import { Effect } from "effect";
import { releaseClusterDomain } from "#/modules/cluster-domain/cluster-domain.server";
import type { Gate, OperationAsked } from "#/modules/approvals/approvals.server";
import { cloudStore, storeTry } from "#/modules/config-store/store-sdk.server";
import type { StoreRefusal, StoreResult } from "#/modules/config-store/store.contract";
import { lacks, requireAccess } from "#/modules/organization/access.server";
import type { Actor, Caller } from "#/modules/identity/actor";
import { deleteEnrollmentTokens, lockOrganizationEnrollment, revokeOrganizationPairing } from "#/modules/machines/pairing-removal.server";
import { fleetToken, waitingServer } from "#/modules/machines/waiting/tables";
import { and, asc, eq } from "drizzle-orm";
import { member, session } from "#/modules/identity/tables";
import { organizationClusterDomain } from "#/modules/cluster-domain/tables";
import { PloyzProviderError } from "#/modules/runtime/ployz.server";
import { organizationPairing } from "#/modules/runtime/tables";
import { organization, organizationChange } from "./tables";
import { Database } from "#/server/database.server";

/** Removing an Organization as a human approves it, named by the slug `ployz org rm` confirms. */
export const organizationRemovalAsked = ({ id, slug }: { id: string; slug: string }): OperationAsked => ({
  subject: `organization:${id}`,
  verb: "remove",
  name: slug,
  preview: { organization: slug },
  effects: [{ kind: "removes_organization", name: slug, node: id, path: `organizations/${id}` }],
  // Carrying its approval again finishes the removal it began, once a Server that was offline confirms.
  recorded: true,
});

/**
 * `ployz org rm`: remove the Caller's Organization, named by its ID, once it has no Project, whose Environments all left the Servers
 * through the one teardown path. The Store forgets its configuration; then pairing removal clears every device key and
 * Cloud's own key on each Server. A Server that doesn't confirm keeps the Organization, its pairing disabled, until a
 * rerun confirms it; only then do Cloud's rows go. `deleting` wraps the row delete inside that transaction, for a caller
 * with rows of its own to take along or a last check to make under the Organization's lock. `gate` asks a human first
 * once nothing but the Organization stands in the way.
 */
export const removeOrganization = Effect.fn("Organization.remove")(function* <E = never>(
  caller: Pick<Caller, "userId" | "organization" | "role"> & Partial<Pick<Caller, "credential">>,
  organizationId: string,
  { deleting = (drop) => drop, gate }: { deleting?: Deleting<E>; gate?: Gate } = {},
) {
  const { id, slug } = caller.organization;
  if (organizationId !== id) {
    return refused({
      code: "invalid_argument",
      message: `This credential acts in Organization ${slug}, not the one named. No changes made.`,
      details: { next: "ployz org ls" },
    });
  }
  const lacking = lacks({ ...caller, support: null }, "org:delete");
  if (lacking !== null) return refused({ code: "forbidden", message: `${lacking} No changes made.`, details: { permission: "org:delete" } });
  const store = yield* cloudStore;
  // A Project refuses the removal before anyone is asked to approve, here and not at the Store: one removed meanwhile
  // would let the Store remove the Organization unapproved.
  if (gate !== undefined) {
    const { projects } = yield* storeTry(() => store.read(id, { query: "projects" })).pipe(Effect.orDie);
    const [first] = projects;
    if (first !== undefined) return refused(stillHasProjects(projects.map((project) => project.name), first.name));
    const gated = yield* gate(organizationRemovalAsked(caller.organization));
    if (!gated.ok) return refused(gated.refusal);
  }
  const refusal = yield* storeTry(() => store.removeOrganization(id)).pipe(
    Effect.as(null),
    Effect.catchTag("StoreRefused", (error) => Effect.succeed(error.refusal)),
  );
  if (refusal !== null) return refused(refusal);
  const revoked = yield* revokeOrganizationPairing(id);
  const servers = {
    confirmed: revoked.endpoints.filter((endpoint) => endpoint.status === "confirmed").map((endpoint) => endpoint.machineId),
    unconfirmed: revoked.endpoints.filter((endpoint) => endpoint.status !== "confirmed").map((endpoint) => endpoint.machineId),
  };
  if (!revoked.confirmed) return removal({ organization: slug, removed: false, servers });
  const database = yield* Database;
  const domain = yield* database.transaction(dropOrganizationRows(id, deleting));
  // Only once the Organization is gone: its Cluster Domain row cascaded with it, so a retry never releases twice.
  if (domain) yield* releaseClusterDomain(domain);
  return removal({ organization: slug, removed: true, servers });
});

type Deleting<E> = (drop: Effect.Effect<void, never, Database>) => Effect.Effect<void, E, Database>;
type OrganizationRemoval = { organization: string; removed: boolean; servers: { confirmed: string[]; unconfirmed: string[] } };
const removal = (value: OrganizationRemoval): StoreResult<OrganizationRemoval> => ({ ok: true, value });
const refused = (refusal: StoreRefusal): StoreResult<OrganizationRemoval> => ({ ok: false, refusal });
/** The Store's own refusal of an Organization with Projects (`remove_organization` in ployz-store's teardown.rs). */
const stillHasProjects = (names: string[], first: string): StoreRefusal => ({
  code: "conflict",
  message: `This Organization still has Projects (${names.join(", ")}). Remove them first`,
  details: { projects: names, next: { tool: "remove", input: { project: first } } },
});

/** The dashboard's Delete organization: the same removal, by an owner of the Organization named by `organizationSlug`. */
export const removeOrganizationAsMember = Effect.fn("Organization.removeAsMember")(function* (actor: Actor, organizationSlug: string) {
  const access = yield* requireAccess(actor, organizationSlug, "org:delete").pipe(
    // A server function answers every Forbidden with one generic sentence; a refusal reaches the dialog as written.
    Effect.catchTag("Forbidden", (error) => Effect.succeed(refused({ code: "forbidden", message: error.message, details: null }))),
  );
  if ("ok" in access) return access;
  return yield* removeOrganization({ userId: actor.userId, organization: access.organization, role: "owner" }, access.organization.id);
});

/**
 * Cloud's own rows of an Organization whose pairing is gone; returns its Cluster Domain, if any, to release. The
 * cascade follows the Organization enrollment lock order: it locks the rows check-in, Reject and revoke take before it
 * deletes them, and ends open attempts, with their runs' cancel events, before it deletes their tokens.
 */
const dropOrganizationRows = Effect.fn("Organization.dropRows")(function* <E>(organizationId: string, deleting: Deleting<E>) {
  const { drizzle } = yield* Database;
  yield* lockOrganizationEnrollment(organizationId);
  yield* drizzle.select({ id: organization.id }).from(organization).where(eq(organization.id, organizationId)).for("update");
  const [pending] = yield* drizzle.select({ organizationId: organizationPairing.organizationId })
    .from(organizationPairing).where(eq(organizationPairing.organizationId, organizationId));
  if (pending) return yield* new PloyzProviderError({
    operation: "drop Organization rows", cause: "Endpoint revocation is unconfirmed; the removal attempt must be retained.",
  });
  const [domain] = yield* drizzle.select().from(organizationClusterDomain).where(eq(organizationClusterDomain.organizationId, organizationId));
  yield* drizzle.select({ id: fleetToken.id }).from(fleetToken)
    .where(eq(fleetToken.organizationId, organizationId)).orderBy(fleetToken.id).for("update");
  yield* drizzle.select({ id: waitingServer.id }).from(waitingServer)
    .where(eq(waitingServer.organizationId, organizationId)).orderBy(waitingServer.id).for("update");
  yield* deleteEnrollmentTokens(organizationId);
  yield* deleting(drizzle.delete(organization).where(eq(organization.id, organizationId)).pipe(Effect.asVoid, Effect.orDie));
  // The cascade's statement triggers logged the deletes under the Organization's id; the log goes with it.
  yield* drizzle.delete(organizationChange).where(eq(organizationChange.organizationId, organizationId));
  // Sessions in it move to their user's next Organization, or none: /cloud then offers a new one.
  const users = yield* drizzle.selectDistinct({ userId: session.userId }).from(session).where(eq(session.activeOrganizationId, organizationId));
  yield* Effect.forEach(users, ({ userId }) => Effect.gen(function* () {
    const [next] = yield* drizzle.select({ id: organization.id, slug: organization.slug }).from(member)
      .innerJoin(organization, eq(organization.id, member.organizationId))
      .where(eq(member.userId, userId)).orderBy(asc(member.createdAt)).limit(1);
    yield* drizzle.update(session)
      .set({ activeOrganizationId: next?.id ?? null, activeOrganizationSlug: next?.slug ?? null, updatedAt: new Date() })
      .where(and(eq(session.userId, userId), eq(session.activeOrganizationId, organizationId)));
  }));
  return domain ?? null;
});
