import "@tanstack/react-start/server-only";
import { eq } from "drizzle-orm";
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
          .where(eq(tables.environmentSavedStateSnapshot.organizationId, scopeId));
      case "project":
        return yield* database.drizzle.select().from(tables.project)
          .where(eq(tables.project.organizationId, scopeId));
      case "environment":
        return yield* database.drizzle.select().from(tables.environment)
          .where(eq(tables.environment.organizationId, scopeId));
      case "service":
        return yield* database.drizzle.select().from(tables.service)
          .where(eq(tables.service.organizationId, scopeId));
      case "resource_lineage":
        return yield* database.drizzle.select().from(tables.resourceLineage)
          .where(eq(tables.resourceLineage.organizationId, scopeId));
      case "environment_resource":
        return yield* database.drizzle.select().from(tables.environmentResource)
          .where(eq(tables.environmentResource.organizationId, scopeId));
      case "environment_canvas_node_position":
        return yield* database.drizzle.select().from(tables.environmentCanvasNodePosition)
          .where(eq(tables.environmentCanvasNodePosition.organizationId, scopeId));
      case "environment_deployment":
        return yield* database.drizzle.select().from(tables.environmentDeployment)
          .where(eq(tables.environmentDeployment.organizationId, scopeId));
      case "environment_node_config_snapshot":
        return yield* database.drizzle.select().from(tables.environmentNodeConfigSnapshot)
          .where(eq(tables.environmentNodeConfigSnapshot.organizationId, scopeId));
      case "environment_node_introduction":
        return yield* database.drizzle.select().from(tables.environmentNodeIntroduction)
          .where(eq(tables.environmentNodeIntroduction.organizationId, scopeId));
      case "volume_remove_attempt":
        return yield* database.drizzle.select().from(tables.volumeRemoveAttempt)
          .where(eq(tables.volumeRemoveAttempt.organizationId, scopeId));
    }
  });
  return yield* read.pipe(Effect.mapError((cause) => new CollectionReadFailure({ cause })));
});
