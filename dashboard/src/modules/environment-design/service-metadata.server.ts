import "@tanstack/react-start/server-only";
import { and, eq, sql } from "drizzle-orm";
import { Effect } from "effect";
import type { Actor } from "#/modules/identity/actor";
import { Database } from "#/server/database.server";
import { Conflict, NotFound } from "#/server/public-error";
import { withMutationResult } from "#/server/mutation-result.server";
import { requireEnvironmentForActorById, listEnvironmentNodeNameIdentities } from "./authoring-repository.server";
import { loadEnvironmentDocument } from "./working-state-repository.server";
import { isEnvironmentNodeNameTaken, getDuplicateEnvironmentNodeNameMessage } from "./environment-node-names";
import { service } from "./tables";
import type { ServiceMetadataEdit } from "./service-metadata";

/** Serializes with resource creation/deletion without changing Working State. */
export const editServiceMetadata = Effect.fn("EnvironmentDesign.editServiceMetadata")(
  function* (actor: Actor, input: ServiceMetadataEdit) {
    yield* requireEnvironmentForActorById(actor, input);
    return yield* withMutationResult(Effect.gen(function* () {
      const document = yield* loadEnvironmentDocument(input.environmentId, true);
      if (!document.intent.services.some(node => node.id === input.serviceId)) {
        return yield* new NotFound({ message: "Service not found." });
      }
      const { edit } = input;
      if (edit.kind === "rename" && isEnvironmentNodeNameTaken(edit.name,
        yield* listEnvironmentNodeNameIdentities(input.environmentId), { type: "service", id: input.serviceId })) {
        return yield* new Conflict({ message: getDuplicateEnvironmentNodeNameMessage(edit.name) });
      }
      const { drizzle } = yield* Database;
      const [row] = yield* drizzle.update(service)
        .set({ ...(edit.kind === "rename" ? { name: edit.name } : { policy: sql`${service.policy} || ${JSON.stringify(edit.policy)}::jsonb` }), updatedAt: new Date() })
        .where(and(eq(service.id, input.serviceId), eq(service.environmentId, input.environmentId))).returning();
      if (!row) return yield* new NotFound({ message: "Service not found." });
      return row;
    }));
  },
);
