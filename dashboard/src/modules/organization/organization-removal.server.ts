import "@tanstack/react-start/server-only";
import { eq } from "drizzle-orm";
import { Effect } from "effect";
import { releaseClusterDomain } from "#/modules/cluster-domain/cluster-domain.server";
import { callStore } from "#/modules/config-store/config-store.server";
import type { StoreRefusal, StoreResult } from "#/modules/config-store/store.contract";
import { getOrganizationForUserBySlug } from "#/modules/environment-design/workspace-repository.server";
import type { Actor, Caller } from "#/modules/identity/actor";
import { AppConfig } from "#/server/config.server";
import { NotFound } from "#/server/public-error";
import { revokeOrganizationPairing } from "#/modules/machines/pairing-removal.server";
import { project } from "#/modules/project/tables";
import { dropOrganizationRows } from "#/modules/runtime/teardown-activities.server";
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
  const { drizzle } = yield* Database;
  // TODO(#1275): the dashboard's own Projects go with its authoring tables.
  const [dashboardProject] = yield* drizzle.select({ slug: project.slug }).from(project)
    .where(eq(project.organizationId, id)).limit(1);
  if (dashboardProject !== undefined) {
    return refused({
      code: "conflict",
      message: `Project ${dashboardProject.slug} was made in the dashboard: delete it there first. No changes made.`,
      details: { project: dashboardProject.slug },
    });
  }
  const forgotten = yield* callStore(id, caller.userId, { operation: "write", command: { command: "remove_organization" } });
  if (!forgotten.ok) return forgotten;
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
  const config = yield* AppConfig;
  // TODO(#1275): dark in production until the Config Store cutover.
  if (config.nodeEnv === "production") return yield* new NotFound({ message: "Not found." });
  const organization = yield* getOrganizationForUserBySlug(actor.userId, organizationSlug).pipe(Effect.orDie);
  if (!organization) return yield* new NotFound({ message: "Organization not found." });
  return yield* removeOrganization({ userId: actor.userId, organization }, organizationSlug);
});
