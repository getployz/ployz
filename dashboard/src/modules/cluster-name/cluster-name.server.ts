import "@tanstack/react-start/server-only";
import { randomUUID } from "node:crypto";
import { and, eq, isNotNull, isNull, ne, or } from "drizzle-orm";
import type { JsonValue } from "@ployz/sdk";
import { Effect, Schema } from "effect";
import { operationDigest, type OperationAsked } from "#/modules/approvals/approvals.server";
import { type ClusterNameRow, defaultClusterName } from "#/modules/cluster-name/cluster-name";
import { organizationClusterName } from "#/modules/cluster-name/tables";
import { createClusterNameRequestedEvent } from "#/modules/inngest/events";
import { enqueueEvent } from "#/modules/operations/outbox.server";
import { organization } from "#/modules/organization/tables";
import { ServerLacks } from "#/modules/runtime/daemon-compat";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import { rpcErrorCode, rpcErrorMessage } from "#/modules/runtime/ployz.server";
import { organizationPairing } from "#/modules/runtime/tables";
import { Database } from "#/server/database.server";

const SET_LIMIT = "30 seconds";

const columns = {
  id: organizationClusterName.organizationId,
  name: organizationClusterName.name,
  appliedName: organizationClusterName.appliedName,
  refusal: organizationClusterName.refusal,
};

export const readClusterName = Effect.fn("ClusterName.read")(function* (organizationId: string) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select(columns).from(organizationClusterName).where(eq(organizationClusterName.organizationId, organizationId));
  return row satisfies ClusterNameRow | undefined;
});

/** What renaming to `to` asks a human from the CLI or an agent: the private addresses move from the current name. */
export const renameAsked = Effect.fn("ClusterName.asked")(function* (organizationId: string, to: string) {
  const row = yield* readClusterName(organizationId);
  return {
    subject: `cluster:${organizationId}`,
    verb: "set_cluster_name",
    name: to,
    preview: { from: row?.name ?? null, to },
    effects: [],
    alwaysAsks: true,
  } satisfies OperationAsked;
});

const Renamed = Schema.Struct({ to: Schema.String });

/** A pending rename's digest as the name stands now: none once the Cluster holds its name, so a rename since supersedes it. */
export const freshRenameDigest = Effect.fn("ClusterName.freshDigest")(function* (organizationId: string, stored: string, preview: JsonValue) {
  const renamed = Schema.decodeUnknownOption(Renamed)(preview);
  if (renamed._tag === "None") return stored;
  const asked = yield* renameAsked(organizationId, renamed.value.to);
  return asked.preview.from === renamed.value.to ? null : operationDigest(asked);
});

const requestApply = (organizationId: string) =>
  enqueueEvent(createClusterNameRequestedEvent({ id: `cluster-name-${randomUUID()}`, organizationId }));

/** The name the first Server founds the Cluster with: the one saved, else the Organization slug's. */
export const foundingClusterName = Effect.fn("ClusterName.founding")(function* (organizationId: string) {
  const row = yield* readClusterName(organizationId);
  if (row !== undefined) return row.name;
  const { drizzle } = yield* Database;
  const [org] = yield* drizzle.select({ slug: organization.slug }).from(organization).where(eq(organization.id, organizationId));
  return defaultClusterName(org?.slug ?? "");
});

/** Keep `name` as the Cluster's, and ask for a run that sets it on a Server, now or once one is online. */
export const renameCluster = Effect.fn("ClusterName.rename")(function* (organizationId: string, name: string) {
  const database = yield* Database;
  return yield* database.transaction(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const [row] = yield* drizzle.insert(organizationClusterName).values({ organizationId, name })
      .onConflictDoUpdate({ target: organizationClusterName.organizationId, set: { name, appliedName: null, refusal: null, updatedAt: new Date() } })
      .returning(columns);
    yield* requestApply(organizationId);
    return row ?? (yield* Effect.die("an upsert returned no row"));
  }));
});

export const foundClusterName = Effect.fn("ClusterName.found")(function* (organizationId: string) {
  const name = yield* foundingClusterName(organizationId);
  const { drizzle } = yield* Database;
  yield* drizzle.insert(organizationClusterName).values({ organizationId, name })
    .onConflictDoUpdate({ target: organizationClusterName.organizationId, set: { appliedName: null, refusal: null, updatedAt: new Date() } });
});

export type ClusterNameApplied = "current" | "unfounded" | "waiting" | "refused";

const refusalOf = (error: Parameters<typeof rpcErrorCode>[0]) => {
  if (error instanceof ServerLacks) return "Needs a newer Ployz on your servers.";
  return rpcErrorCode(error) === "invalid_argument" ? rpcErrorMessage(error) ?? "The servers refused this name." : null;
};

/**
 * Set the saved name on the Cluster through any Server that answers, and record it once one confirms. A name saved
 * meanwhile stays pending: the write lands only on the name it set. `waiting` means try again later; a refusal is
 * tried again by the hourly retry, since an upgrade clears one.
 */
export const applyClusterName = Effect.fn("ClusterName.apply")(function* (organizationId: string) {
  const row = yield* readClusterName(organizationId);
  if (row === undefined || row.appliedName === row.name) return "current" satisfies ClusterNameApplied;
  const session = yield* (yield* OrganizationRuntime).open(organizationId);
  if (session.status === "no_connection") return "unfounded" satisfies ClusterNameApplied;
  if (session.status !== "connected") return "waiting" satisfies ClusterNameApplied;
  const answer = yield* session.connected.setClusterName(row.name).pipe(Effect.timeout(SET_LIMIT), Effect.result);
  const { drizzle } = yield* Database;
  const same = and(eq(organizationClusterName.organizationId, organizationId), eq(organizationClusterName.name, row.name));
  if (answer._tag === "Success") {
    const [written] = yield* drizzle.update(organizationClusterName).set({ appliedName: row.name, refusal: null, updatedAt: new Date() })
      .where(same).returning({ id: organizationClusterName.organizationId });
    return (written === undefined ? "waiting" : "current") satisfies ClusterNameApplied;
  }
  const refusal = answer.failure._tag === "TimeoutError" ? null : refusalOf(answer.failure);
  if (refusal === null) {
    yield* Effect.logWarning("No Server answered, so the Cluster Name stays pending.", answer.failure).pipe(Effect.annotateLogs({ organizationId }));
    return "waiting" satisfies ClusterNameApplied;
  }
  yield* drizzle.update(organizationClusterName).set({ refusal, updatedAt: new Date() }).where(same);
  return "refused" satisfies ClusterNameApplied;
}, Effect.scoped);

/** Organizations whose founded Cluster doesn't hold its saved name yet, for the hourly retry. */
export const listUnappliedClusterNames = Effect.fn("ClusterName.listUnapplied")(function* () {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.select({ id: organizationClusterName.organizationId }).from(organizationClusterName)
    .innerJoin(organizationPairing, eq(organizationPairing.organizationId, organizationClusterName.organizationId))
    .where(and(
      isNotNull(organizationPairing.founderMachineId),
      isNull(organizationPairing.removalStartedAt),
      or(isNull(organizationClusterName.appliedName), ne(organizationClusterName.appliedName, organizationClusterName.name)),
    ));
  return rows.map(({ id }) => id);
});
