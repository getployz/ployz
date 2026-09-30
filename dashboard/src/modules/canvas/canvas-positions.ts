import { createSelectSchema } from "drizzle-orm/effect-schema";
import { Schema } from "effect";
import { environmentCanvasNodePosition } from "./tables";

const row = createSelectSchema(environmentCanvasNodePosition);

/** A node's place on its Environment's canvas, as the Org Store syncs it. */
export const canvasPositionSchema = Schema.Struct({
  id: row.fields.id,
  environmentId: row.fields.environmentId,
  resourceType: row.fields.resourceType,
  resourceId: row.fields.resourceId,
  x: row.fields.x,
  y: row.fields.y,
  createdAt: row.fields.createdAt,
  updatedAt: row.fields.updatedAt,
});
export type CanvasPosition = typeof canvasPositionSchema.Type;

export const canvasResourceTypes = ["service", "volume"] as const;

/** One node's place: the Org Store collection's key. */
export function canvasPositionKey(item: Pick<CanvasPosition, "resourceType" | "resourceId">) {
  return `${item.resourceType}:${item.resourceId}`;
}

export const updateCanvasPositionSchema = Schema.Struct({
  organizationSlug: Schema.NonEmptyString,
  environmentId: Schema.String.check(Schema.isUUID()),
  resourceType: Schema.Literals(canvasResourceTypes),
  resourceId: Schema.String.check(Schema.isUUID()),
  x: Schema.Finite,
  y: Schema.Finite,
});
export type UpdateCanvasPositionInput = typeof updateCanvasPositionSchema.Type;
