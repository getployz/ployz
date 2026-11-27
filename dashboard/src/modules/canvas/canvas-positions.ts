import { createSelectSchema } from "drizzle-orm/effect-schema";
import { Schema } from "effect";
import { environmentCanvasNodePosition } from "./tables";

const row = createSelectSchema(environmentCanvasNodePosition);

/** A node's place on its Environment's canvas, as the Org Store syncs it. */
export const canvasPositionSchema = Schema.Struct({
  id: row.fields.id,
  organizationId: row.fields.organizationId,
  environmentId: row.fields.environmentId,
  resourceType: row.fields.resourceType,
  resourceId: row.fields.resourceId,
  x: row.fields.x,
  y: row.fields.y,
  createdAt: row.fields.createdAt,
  updatedAt: row.fields.updatedAt,
});
export type CanvasPosition = typeof canvasPositionSchema.Type;

export const canvasResourceTypes = ["service", "volume", "config"] as const;
/** What has a place: the nodes a person drags, and a Branch's Live Nodes, which hold still where they were first put. */
export const canvasPlaceTypes = [...canvasResourceTypes, "live"] as const;

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

/** The most nodes one canvas places at once: every node it draws on its first look. */
export const MAX_PLACED_NODES = 500;

export const placeCanvasNodesSchema = Schema.Struct({
  organizationSlug: Schema.NonEmptyString,
  environmentId: Schema.String.check(Schema.isUUID()),
  nodes: Schema.Array(Schema.Struct({
    resourceType: Schema.Literals(canvasPlaceTypes),
    resourceId: Schema.NonEmptyString.check(Schema.isMaxLength(255)),
    x: Schema.Finite,
    y: Schema.Finite,
  })).check(Schema.isMinLength(1), Schema.isMaxLength(MAX_PLACED_NODES)),
});
export type PlaceCanvasNodesInput = typeof placeCanvasNodesSchema.Type;
export type CanvasPlacement = PlaceCanvasNodesInput["nodes"][number];
