import "@tanstack/react-start/server-only";
import { eq } from "drizzle-orm";
import { Effect } from "effect";
import type { Actor } from "#/modules/identity/actor";
import { requireOrganizationForMember } from "#/modules/organization/organization-state.server";
import { withMutationResult } from "#/server/mutation-result.server";
import { Database } from "#/server/database.server";
import type { UpdateCanvasPositionInput } from "./canvas-positions";
import { environmentCanvasNodePosition } from "./tables";

/**
 * Puts a node where the canvas dropped it. Positions are presentation, kept by Organization: a node is placed before
 * the Store creates it, in an Environment only the Store knows.
 */
export const updateCanvasPosition = Effect.fn("Canvas.updatePosition")(function* (actor: Actor, input: UpdateCanvasPositionInput) {
  const organization = yield* requireOrganizationForMember(actor, input.organizationSlug);
  return yield* withMutationResult(Effect.gen(function* () {
    const database = yield* Database;
    const x = Math.round(input.x);
    const y = Math.round(input.y);
    const now = new Date();
    const [row] = yield* database.drizzle
      .insert(environmentCanvasNodePosition)
      .values({ organizationId: organization.id, environmentId: input.environmentId, resourceType: input.resourceType,
        resourceId: input.resourceId, x, y, updatedAt: now })
      .onConflictDoUpdate({
        target: [environmentCanvasNodePosition.environmentId, environmentCanvasNodePosition.resourceType, environmentCanvasNodePosition.resourceId],
        // Only its own Organization moves it.
        setWhere: eq(environmentCanvasNodePosition.organizationId, organization.id),
        set: { x, y, updatedAt: now },
      })
      .returning();
    if (!row) return yield* Effect.die("PostgreSQL did not return the canvas position.");
    return row;
  }));
});
