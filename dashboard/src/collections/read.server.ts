import "@tanstack/react-start/server-only";
import type { AnyPgColumn } from "drizzle-orm/pg-core";
import { and, eq, inArray, or } from "drizzle-orm";
import { Data, Effect } from "effect";
import type { CollectionReadInput } from "./read.contract";
import * as tables from "#/db/schema";
import type { Actor } from "#/modules/identity/actor";
import { getOrganizationForUserBySlug } from "#/modules/environment-design/workspace-repository.server";
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
export class CollectionReadInvalid extends Data.TaggedError("CollectionReadInvalid")<{
  readonly message: string;
}> {
  readonly publicErrorCategory = "validation" as const;
}

export const readCollection = Effect.fn("Collections.read")(function* (
  actor: Actor,
  data: CollectionReadInput,
) {
  if (actor.userId !== data.userId) {
    return yield* new CollectionReadDenied({ message: "Collection not found." });
  }
  let scopeId = actor.userId;
  if (data.table !== "github_repository_cache") {
    if (!data.organizationSlug) {
      return yield* new CollectionReadInvalid({ message: "organizationSlug is required." });
    }
    const organization = yield* getOrganizationForUserBySlug(actor.userId, data.organizationSlug)
      .pipe(Effect.mapError((cause) => new CollectionReadFailure({ cause })));
    if (!organization) {
      return yield* new CollectionReadDenied({ message: "Organization not found." });
    }
    scopeId = organization.id;
  }
  const database = yield* Database;
  const environmentIds = database.drizzle.select({ id: tables.environment.id }).from(tables.environment)
    .where(and(eq(tables.environment.organizationId, scopeId), eq(tables.environment.namespace, data.environmentSlug ?? "")));
  const environmentFilter = (column: AnyPgColumn) => data.environmentSlug ? inArray(column, environmentIds) : undefined;
  const read = Effect.gen(function* () {
    switch (data.table) {
      case "github_repository_cache":
        return yield* database.drizzle.select().from(tables.githubRepositoryCache)
          .where(eq(tables.githubRepositoryCache.userId, scopeId));
      case "environment_saved_state_snapshot":
        return yield* database.drizzle.select({
          id: tables.environmentSavedStateSnapshot.id,
          organizationId: tables.environmentSavedStateSnapshot.organizationId,
          environmentId: tables.environmentSavedStateSnapshot.environmentId,
        }).from(tables.environmentSavedStateSnapshot)
          .where(and(eq(tables.environmentSavedStateSnapshot.organizationId, scopeId), environmentFilter(tables.environmentSavedStateSnapshot.environmentId)));
      case "environment_summary":
        return yield* database.drizzle.select({
          id: tables.environment.id, projectId: tables.environment.projectId, organizationId: tables.environment.organizationId,
          name: tables.environment.name, namespace: tables.environment.namespace, createdAt: tables.environment.createdAt,
        }).from(tables.environment).where(eq(tables.environment.organizationId, scopeId));
      case "project_preference":
        return yield* database.drizzle.select({ id: tables.userProjectPreference.projectId, environmentId: tables.userProjectPreference.environmentId })
          .from(tables.userProjectPreference).where(and(eq(tables.userProjectPreference.organizationId, scopeId), eq(tables.userProjectPreference.userId, actor.userId)));
      case "project":
        return yield* database.drizzle.select().from(tables.project)
          .where(eq(tables.project.organizationId, scopeId));
      case "environment":
        return yield* database.drizzle.select().from(tables.environment)
          .where(and(eq(tables.environment.organizationId, scopeId), environmentFilter(tables.environment.id)));
      case "service":
        return yield* database.drizzle.select().from(tables.service)
          .where(and(eq(tables.service.organizationId, scopeId), environmentFilter(tables.service.environmentId)));
      case "resource_lineage":
        return yield* database.drizzle.select().from(tables.resourceLineage)
          .where(and(eq(tables.resourceLineage.organizationId, scopeId), data.environmentSlug
            ? or(
              inArray(tables.resourceLineage.id, database.drizzle.select({ id: tables.environmentResource.lineageId }).from(tables.environmentResource).where(environmentFilter(tables.environmentResource.environmentId))),
              inArray(tables.resourceLineage.id, database.drizzle.select({ id: tables.service.lineageId }).from(tables.service).where(environmentFilter(tables.service.environmentId))),
            )
            : undefined));
      case "environment_resource":
        return yield* database.drizzle.select().from(tables.environmentResource)
          .where(and(eq(tables.environmentResource.organizationId, scopeId), environmentFilter(tables.environmentResource.environmentId)));
      case "environment_canvas_node_position":
        return yield* database.drizzle.select().from(tables.environmentCanvasNodePosition)
          .where(and(eq(tables.environmentCanvasNodePosition.organizationId, scopeId), environmentFilter(tables.environmentCanvasNodePosition.environmentId)));
      case "environment_deployment":
        return yield* database.drizzle.select().from(tables.environmentDeployment)
          .where(and(eq(tables.environmentDeployment.organizationId, scopeId), environmentFilter(tables.environmentDeployment.environmentId)));
      case "environment_node_config_snapshot":
        return yield* database.drizzle.select().from(tables.environmentNodeConfigSnapshot)
          .where(and(eq(tables.environmentNodeConfigSnapshot.organizationId, scopeId), environmentFilter(tables.environmentNodeConfigSnapshot.environmentId)));
      case "environment_node_introduction":
        return yield* database.drizzle.select().from(tables.environmentNodeIntroduction)
          .where(and(eq(tables.environmentNodeIntroduction.organizationId, scopeId), environmentFilter(tables.environmentNodeIntroduction.environmentId)));
      case "volume_remove_attempt":
        return yield* database.drizzle.select().from(tables.volumeRemoveAttempt)
          .where(and(eq(tables.volumeRemoveAttempt.organizationId, scopeId), environmentFilter(tables.volumeRemoveAttempt.environmentId)));
    }
  });
  return yield* read.pipe(Effect.mapError((cause) => new CollectionReadFailure({ cause })));
});
