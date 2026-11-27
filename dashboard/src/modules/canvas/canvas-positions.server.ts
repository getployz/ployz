import "@tanstack/react-start/server-only";
import { and, eq, inArray, sql } from "drizzle-orm";
import { Effect } from "effect";
import type { Actor } from "#/modules/identity/actor";
import { requireAccess } from "#/modules/organization/access.server";
import { withMutationResult } from "#/server/mutation-result.server";
import { Database } from "#/server/database.server";
import { canvasPositionKey, type PlaceCanvasNodesInput, type UpdateCanvasPositionInput } from "./canvas-positions";
import { environmentCanvasNodePosition } from "./tables";

/**
 * Puts a node where the canvas dropped it. Positions are presentation, kept by Organization: a node is placed before
 * the Store creates it, in an Environment only the Store knows.
 */
export const updateCanvasPosition = Effect.fn("Canvas.updatePosition")(function* (actor: Actor, input: UpdateCanvasPositionInput) {
  const { organization } = yield* requireAccess(actor, input.organizationSlug, "project:edit");
  return yield* withMutationResult(Effect.gen(function* () {
    const database = yield* Database;
    const x = Math.round(input.x);
    const y = Math.round(input.y);
    const [row] = yield* database.drizzle
      .insert(environmentCanvasNodePosition)
      .values({ organizationId: organization.id, environmentId: input.environmentId, resourceType: input.resourceType,
        resourceId: input.resourceId, x, y, updatedAt: sql`now()` })
      .onConflictDoUpdate({
        target: [environmentCanvasNodePosition.environmentId, environmentCanvasNodePosition.resourceType, environmentCanvasNodePosition.resourceId],
        setWhere: eq(environmentCanvasNodePosition.organizationId, organization.id),
        set: { x, y, updatedAt: sql`now()` },
      })
      .returning();
    if (!row) return yield* Effect.die("PostgreSQL did not return the canvas position.");
    return row;
  }));
});

/**
 * Gives each node the canvas first drew without a place the place it drew it at, in one write. A node that has a place
 * keeps it: a second tab placing the same nodes, or a person's drag, is never overwritten. The answer is each node's
 * stored place, whoever wrote it.
 */
export const placeCanvasNodes = Effect.fn("Canvas.placeNodes")(function* (actor: Actor, input: PlaceCanvasNodesInput) {
  const { organization } = yield* requireAccess(actor, input.organizationSlug, "project:edit");
  return yield* withMutationResult(Effect.gen(function* () {
    const database = yield* Database;
    yield* database.drizzle
      .insert(environmentCanvasNodePosition)
      .values(input.nodes.map((node) => ({ organizationId: organization.id, environmentId: input.environmentId,
        resourceType: node.resourceType, resourceId: node.resourceId, x: Math.round(node.x), y: Math.round(node.y) })))
      .onConflictDoNothing({
        target: [environmentCanvasNodePosition.environmentId, environmentCanvasNodePosition.resourceType, environmentCanvasNodePosition.resourceId],
      });
    const asked = new Set(input.nodes.map(canvasPositionKey));
    const rows = yield* database.drizzle.select().from(environmentCanvasNodePosition).where(and(
      eq(environmentCanvasNodePosition.organizationId, organization.id),
      eq(environmentCanvasNodePosition.environmentId, input.environmentId),
      inArray(environmentCanvasNodePosition.resourceId, input.nodes.map((node) => node.resourceId)),
    ));
    return rows.filter((row) => asked.has(canvasPositionKey(row)));
  }));
});
