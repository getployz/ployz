import "@tanstack/react-start/server-only";
import { eq } from "drizzle-orm";
import { Effect, Redacted } from "effect";
import { releaseHostedDomain, reserveHostedDomain } from "#/modules/cluster-domain/hosted-dns.server";
import { organizationClusterDomain, type OrganizationClusterDomain } from "#/modules/cluster-domain/tables";
import type { Actor } from "#/modules/identity/actor";
import { sendInngestEvent } from "#/modules/inngest/client";
import { createClusterDomainSyncRequestedEvent } from "#/modules/inngest/events";
import { organization as schemaOrganization } from "#/modules/organization/tables";
import { requireInfrastructureOrganization } from "#/modules/runtime/organization-access.server";
import { AppConfig } from "#/server/config.server";
import { Database } from "#/server/database.server";
import { Conflict, NotFound } from "#/server/public-error";
import { SecretEncryption } from "#/utils/encrypted-secret.server";

const organizationNotFound = () => new NotFound({ message: "The organization was not found." });

/** The Organization's Cluster Domain row, or null when no name is reserved yet. */
export const loadClusterDomain = Effect.fn("ClusterDomain.load")(function* (organizationId: string) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select().from(organizationClusterDomain)
    .where(eq(organizationClusterDomain.organizationId, organizationId)).limit(1);
  return row ?? null;
});

/**
 * The Organization's Cluster Domain row, reserving one from `PLOYZ_HOSTED_DNS_URL` with the
 * Organization slug as the preferred label when there is none. Fails with `HostedDnsError`
 * when Hosted DNS grants nothing.
 */
export const reserveClusterDomain = Effect.fn("ClusterDomain.reserve")(function* (organizationId: string) {
  const existing = yield* loadClusterDomain(organizationId);
  if (existing) return existing;
  const { drizzle } = yield* Database;
  const [owner] = yield* drizzle.select({ slug: schemaOrganization.slug }).from(schemaOrganization)
    .where(eq(schemaOrganization.id, organizationId)).limit(1);
  if (!owner) return yield* organizationNotFound();
  const { hostedDnsUrl, hostedDnsMintKey } = (yield* AppConfig).ployz;
  const endpoint = hostedDnsUrl.href;
  const granted = yield* reserveHostedDomain({
    endpoint,
    preferred: owner.slug,
    mintKey: hostedDnsMintKey === undefined ? undefined : Redacted.value(hostedDnsMintKey),
  });
  const now = new Date();
  const [inserted] = yield* drizzle.insert(organizationClusterDomain).values({
    organizationId,
    endpoint,
    name: granted.name,
    encryptedToken: (yield* SecretEncryption).encrypt(granted.token),
    reservedAt: now,
    leaseRenewedAt: now,
  }).onConflictDoNothing().returning();
  if (inserted) return inserted;
  // A concurrent reserve won; hand our name back rather than let it wait for the reaper.
  yield* releaseHostedDomain({ endpoint, name: granted.name, token: granted.token }).pipe(Effect.ignore);
  return (yield* loadClusterDomain(organizationId)) ?? (yield* organizationNotFound());
});

/** Retires the name at the endpoint that granted it. Try-once: a failure is logged, and the lease reaps the name. */
export const releaseClusterDomain = Effect.fn("ClusterDomain.release")(function* (
  row: Pick<OrganizationClusterDomain, "endpoint" | "name" | "encryptedToken">,
) {
  const token = (yield* SecretEncryption).decrypt(row.encryptedToken);
  yield* releaseHostedDomain({ endpoint: row.endpoint, name: row.name, token }).pipe(
    Effect.catch((error) => Effect.logWarning("Cluster Domain release failed; the Hosted DNS lease will reap it.", error)),
  );
});

/** Server Settings' Publish now: reserves the name when the Organization has none, then requests a sync. */
export const publishClusterDomainNow = Effect.fn("ClusterDomain.publishNow")(function* (
  actor: Actor,
  input: { readonly organizationSlug: string },
) {
  const { id } = yield* requireInfrastructureOrganization(actor, input.organizationSlug);
  const row = yield* reserveClusterDomain(id).pipe(Effect.catchTag("HostedDnsError", (error) =>
    Effect.logWarning("Cluster Domain reservation failed.", error).pipe(
      Effect.andThen(Effect.fail(new Conflict({ message: "Hosted DNS is unreachable. Try again shortly." }))),
    )));
  yield* sendInngestEvent(createClusterDomainSyncRequestedEvent({ organizationId: id })).pipe(
    Effect.catchTag("InngestEventSendError", (error) => Effect.logWarning("Cluster Domain sync request failed.", error).pipe(
      Effect.andThen(Effect.fail(new Conflict({ message: "The records couldn’t be published. Try again shortly." }))),
    )),
  );
  return { name: row.name };
});
