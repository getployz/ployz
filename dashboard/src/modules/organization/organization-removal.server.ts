import "@tanstack/react-start/server-only";
import { Effect } from "effect";
import { releaseClusterDomain } from "#/modules/cluster-domain/cluster-domain.server";
import { cloudStore, storeTry } from "#/modules/config-store/store-sdk.server";
import type { StoreRefusal, StoreResult } from "#/modules/config-store/store.contract";
import { getOrganizationForUserBySlug } from "#/modules/organization/organization-state.server";
import type { Actor, Caller } from "#/modules/identity/actor";
import { NotFound } from "#/server/public-error";
import { revokeOrganizationPairing } from "#/modules/machines/pairing-removal.server";
import { eq } from "drizzle-orm";
import { session } from "#/modules/identity/tables";
import { organizationClusterDomain } from "#/modules/cluster-domain/tables";
import { machineEnrollmentToken } from "#/modules/machines/tables";
import { PloyzProviderError } from "#/modules/runtime/ployz.server";
import { organizationPairing } from "#/modules/runtime/tables";
import { organization } from "./tables";
import { Database } from "#/server/database.server";

/**
 * `ployz org rm`: remove the Caller's Organization once it has no Project, whose Environments all left the Servers
 * through the one teardown path. The Store forgets its configuration; then pairing removal clears every device key and
 * Cloud's own key on each Server. A Server that doesn't confirm keeps the Organization, its pairing disabled, until a
 * rerun confirms it; only then do Cloud's rows go.
 */
export const removeOrganization = Effect.fn("Organization.remove")(function* (caller: Pick<Caller, "userId" | "organization">, slug: string) {
  const { id, slug: current } = caller.organization;
  if (slug !== current) {
    return refused({
      code: "invalid_argument",
      message: `This credential acts in Organization ${current}, not ${slug}. No changes made.`,
      details: { next: `ployz org use ${slug}` },
    });
  }
  const refusal = yield* cloudStore.pipe(
    Effect.flatMap((store) => storeTry(() => store.removeOrganization(id))),
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
  const domain = yield* database.transaction(dropOrganizationRows(id));
  // Only once the Organization is gone: its Cluster Domain row cascaded with it, so a retry never releases twice.
  if (domain) yield* releaseClusterDomain(domain);
  return removal({ organization: slug, removed: true, servers });
});

type OrganizationRemoval = { organization: string; removed: boolean; servers: { confirmed: string[]; unconfirmed: string[] } };
const removal = (value: OrganizationRemoval): StoreResult<OrganizationRemoval> => ({ ok: true, value });
const refused = (refusal: StoreRefusal): StoreResult<OrganizationRemoval> => ({ ok: false, refusal });

/** The dashboard's Delete organization: the same removal, by a member of the Organization named by `organizationSlug`. */
export const removeOrganizationAsMember = Effect.fn("Organization.removeAsMember")(function* (actor: Actor, organizationSlug: string) {
  const organization = yield* getOrganizationForUserBySlug(actor.userId, organizationSlug).pipe(Effect.orDie);
  if (!organization) return yield* new NotFound({ message: "Organization not found." });
  return yield* removeOrganization({ userId: actor.userId, organization }, organizationSlug);
});

/** Cloud's own rows of an Organization whose pairing is gone; returns its Cluster Domain, if any, to release. */
const dropOrganizationRows = Effect.fn("Organization.dropRows")(function* (organizationId: string) {
  const { drizzle } = yield* Database;
  const [pending] = yield* drizzle.select({ organizationId: organizationPairing.organizationId })
    .from(organizationPairing).where(eq(organizationPairing.organizationId, organizationId));
  if (pending) return yield* new PloyzProviderError({
    operation: "drop Organization rows", cause: "Endpoint revocation is unconfirmed; the removal attempt must be retained.",
  });
  const [domain] = yield* drizzle.select().from(organizationClusterDomain).where(eq(organizationClusterDomain.organizationId, organizationId));
  yield* drizzle.delete(machineEnrollmentToken).where(eq(machineEnrollmentToken.organizationId, organizationId));
  yield* drizzle.delete(organization).where(eq(organization.id, organizationId));
  // Sessions in it act nowhere now: /cloud offers the next Organization or a new one.
  yield* drizzle.update(session).set({ activeOrganizationId: null, activeOrganizationSlug: null })
    .where(eq(session.activeOrganizationId, organizationId));
  return domain ?? null;
});
