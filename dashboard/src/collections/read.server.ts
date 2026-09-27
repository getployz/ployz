import "@tanstack/react-start/server-only";
import { and, eq, inArray, sql } from "drizzle-orm";
import type { EffectDrizzleQueryError } from "drizzle-orm/effect-core";
import type { AnyPgColumn, PgTable } from "drizzle-orm/pg-core";
import { Data, Effect } from "effect";
import { changeNameSources } from "./change-sources";
import type { CollectionRead, CollectionReadInput } from "./read.contract";
import * as tables from "#/db/schema";
import type { Actor } from "#/modules/identity/actor";
import { pairingEnrollmentStatus, type OrganizationEnrollmentRow } from "#/modules/machines/enrollment";
import { changeSources } from "#/modules/organization/change-log.sources";
import type { ClusterDomainRow } from "#/modules/cluster-domain/cluster-domain";
import type { BuildOrderRow } from "#/modules/deployments/build-order";
import type { BranchRow, ConditionalSaveRow } from "#/modules/pr-environments/tables";
import { deploymentRowColumns, orgStoreDeploymentSlice } from "#/modules/deployments/deployment-row.server";
import { readChangeWindow, type OrganizationChangeLogFailure } from "#/modules/organization/change-log.server";
import { getOrganizationForUserBySlug } from "#/modules/environment-design/workspace-repository.server";
import { withoutSealedCiphertext } from "#/modules/environment-design/saved-intent";
import type { JsonValue } from "#/db/tables";
import { Database } from "#/server/database.server";

export class CollectionReadFailure extends Data.TaggedError("CollectionReadFailure")<{
  readonly cause: unknown;
}> {
  readonly publicErrorCategory = "internal" as const;
}
export class CollectionReadDenied extends Data.TaggedError("CollectionReadDenied")<{
  readonly message: string;
}> {
  readonly publicErrorCategory = "not-found" as const;
}

const withoutFingerprints = <Value>(value: Value): Value =>
  // SAFETY: a JSON round trip of JSON data returns the same shape minus the dropped keys.
  JSON.parse(JSON.stringify(value, (key: string, entry: JsonValue) => key === "fingerprint" || key === "valueFingerprint" ? undefined : entry)) as Value;

export const readCollection = Effect.fn("Collections.read")(function* (
  actor: Actor,
  data: CollectionReadInput,
) {
  if (actor.userId !== data.userId) {
    return yield* new CollectionReadDenied({ message: "Collection not found." });
  }
  const organization = yield* getOrganizationForUserBySlug(actor.userId, data.organizationSlug)
    .pipe(Effect.mapError((cause) => new CollectionReadFailure({ cause })));
  if (!organization) {
    return yield* new CollectionReadDenied({ message: "Organization not found." });
  }
  const database = yield* Database;
  // `keys` narrows a read to the rows its change window names, by the key its key table logs.
  const readRows = (keys?: string[]) => Effect.gen(function* () {
    const keyColumns = changeSources[changeNameSources[data.table][0]].key;
    const scoped = (table: PgTable & { organizationId: AnyPgColumn }) => and(
      eq(table.organizationId, organization.id),
      keys && inArray(sql.join(keyColumns.map((column) => sql`${table}.${sql.identifier(column)}`), sql` || ':' || `), keys),
    );
    switch (data.table) {
      case "environment_summary":
        return yield* database.drizzle.select({
          id: tables.environment.id, projectId: tables.environment.projectId, organizationId: tables.environment.organizationId,
          name: tables.environment.name, namespace: tables.environment.namespace, createdAt: tables.environment.createdAt,
        }).from(tables.environment).where(scoped(tables.environment));
      case "project":
        return yield* database.drizzle.select().from(tables.project).where(scoped(tables.project));
      case "environment":
        return yield* database.drizzle.select().from(tables.environment).where(scoped(tables.environment));
      // A base is core's redacted configuration; stripping again keeps sealed ciphertext on the server regardless.
      case "environment_branch": {
        const rows: BranchRow[] = (yield* database.drizzle.select({ branch: tables.environmentBranch, pullRequest: tables.prEnvironment })
          .from(tables.environmentBranch)
          .leftJoin(tables.prEnvironment, eq(tables.prEnvironment.environmentId, tables.environmentBranch.environmentId))
          .where(scoped(tables.environmentBranch)))
          .map(({ branch, pullRequest }) => ({ ...branch, base: withoutSealedCiphertext(branch.base), pullRequest }));
        return rows;
      }
      case "pr_environment_plan":
        return yield* database.drizzle.select().from(tables.prEnvironmentPlan).where(scoped(tables.prEnvironmentPlan));
      // Sealed picks and the landing copy stay on the server; held rows go without secret fingerprints.
      case "conditional_save": {
        const save = tables.conditionalSave;
        const rows: ConditionalSaveRow[] = (yield* database.drizzle.select({
          id: save.id, organizationId: save.organizationId, projectId: save.projectId, prEnvironmentId: save.prEnvironmentId,
          repositoryId: save.repositoryId, prNumber: save.prNumber, destinationEnvironmentId: save.destinationEnvironmentId,
          rows: save.rows, workingRevision: save.workingRevision, targetBranch: save.targetBranch,
          approvedBy: tables.user.name, approvedAt: save.approvedAt, state: save.state, landedSavedStateId: save.landedSavedStateId,
        }).from(save).leftJoin(tables.user, eq(tables.user.id, save.approvedByUserId)).where(scoped(save)))
          .map((row) => ({ ...row, rows: withoutFingerprints(row.rows) }));
        return rows;
      }
      case "service":
        return yield* database.drizzle.select().from(tables.service).where(scoped(tables.service));
      case "resource_lineage":
        return yield* database.drizzle.select().from(tables.resourceLineage).where(scoped(tables.resourceLineage));
      case "environment_resource":
        return yield* database.drizzle.select().from(tables.environmentResource)
          .where(scoped(tables.environmentResource));
      case "environment_canvas_node_position":
        return yield* database.drizzle.select().from(tables.environmentCanvasNodePosition)
          .where(scoped(tables.environmentCanvasNodePosition));
      // ponytail: an attempt that leaves the slice mid-session (a newer one landed) stays in this browser until the
      // next full read: a delta read of its key returns no row and no delete. Bounded by what one session does.
      case "environment_deployment":
        return yield* database.drizzle.select(deploymentRowColumns).from(tables.environmentDeployment)
          .where(and(scoped(tables.environmentDeployment), orgStoreDeploymentSlice(organization.id)));
      // Sealed variable ciphertext stays on the server; deploy resolution reads the full rows.
      case "environment_node_introduction":
        return (yield* database.drizzle.select().from(tables.environmentNodeIntroduction)
          .where(scoped(tables.environmentNodeIntroduction))).map((row) => ({ ...row, config: withoutSealedCiphertext(row.config) }));
      case "organization_enrollment": {
        // The pairing row holds the encrypted pairing secret; expose only the derived status.
        const pairings = yield* database.drizzle.select({
          id: tables.organizationPairing.organizationId,
          founderMachineId: tables.organizationPairing.founderMachineId,
        }).from(tables.organizationPairing).where(scoped(tables.organizationPairing));
        return pairings.map((row): OrganizationEnrollmentRow => ({ id: row.id, status: pairingEnrollmentStatus(row.founderMachineId) }));
      }
      case "organization_cluster_domain": {
        // The token and certificate key stay on the server.
        const domain = tables.organizationClusterDomain;
        const rows: ClusterDomainRow[] = yield* database.drizzle.select({
          id: domain.organizationId, name: domain.name, recordsSyncedAt: domain.recordsSyncedAt, traffic: domain.traffic,
          certificateNotAfter: domain.certificateNotAfter, checkedAt: domain.checkedAt,
        }).from(domain).where(scoped(domain));
        return rows;
      }
      case "organization_build_order": {
        // Always one row: an Organization that never chose has none saved, and the default applies.
        const [saved] = yield* database.drizzle.select({ buildOrder: tables.organizationBuildOrder.buildOrder })
          .from(tables.organizationBuildOrder).where(scoped(tables.organizationBuildOrder));
        const rows: BuildOrderRow[] = [{ id: organization.id, buildOrder: saved?.buildOrder ?? null }];
        return rows;
      }
    }
  });
  type Row = Effect.Success<ReturnType<typeof readRows>>[number];
  const read = Effect.gen(function* (): Effect.fn.Return<CollectionRead<Row>, EffectDrizzleQueryError | OrganizationChangeLogFailure, Database> {
    // The window is read before the rows, so the rows are at least as new as its cursor.
    const window = yield* readChangeWindow({ organizationId: organization.id, since: data.since, sourceTables: changeNameSources[data.table] });
    if (window.kind === "full") return { full: true, rows: yield* readRows(), cursor: window.cursor };
    // Deleted keys are re-read too: a key a filtered read shares with another user's row
    // (project preferences) can be deleted there and still exist here. The client drops, then upserts.
    const keys = [...new Set([...window.changed, ...window.deleted])];
    const rows = keys.length === 0 ? [] : yield* readRows(keys);
    return { full: false, rows, deleted: window.deleted, cursor: window.cursor };
  });
  return yield* read.pipe(Effect.mapError((cause) => new CollectionReadFailure({ cause })));
});
