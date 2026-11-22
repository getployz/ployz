import "@tanstack/react-start/server-only";
import { Effect } from "effect";
import { canvasPositions } from "./canvas.collection";
import { environmentCanvasNodePosition } from "./tables";
import { defineProjection } from "#/collections/change-log/projection.server";
import { Database } from "#/server/database.server";

export const canvasPositionsProjection = defineProjection(canvasPositions, (scoped) => Effect.gen(function* () {
  const database = yield* Database;
  return yield* database.drizzle.select().from(environmentCanvasNodePosition).where(scoped(environmentCanvasNodePosition));
}));
