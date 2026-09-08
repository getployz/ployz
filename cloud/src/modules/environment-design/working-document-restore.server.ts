import "@tanstack/react-start/server-only";
import { restoreEnvironmentNode } from "@ployz/sdk/config";
import { and, eq } from "drizzle-orm";
import { Effect } from "effect";
import type { Actor } from "#/modules/identity/actor";
import { environmentDeployment } from "#/modules/deployments/tables";
import { Database } from "#/server/database.server";
import { withMutationReceipt } from "#/server/mutation-receipt.server";
import { Conflict, NotFound } from "#/server/public-error";
import { requireEnvironmentForActorById } from "./authoring-repository.server";
import { emptyEnvironmentIntent, type SavedEnvironmentIntent } from "./saved-intent";
import { loadEnvironmentSavedIntentById } from "./saved-state-repository.server";
import { loadEnvironmentDocument, requireDocumentRevision, writeEnvironmentDocument } from "./working-state-repository.server";
import type { RestoreWorkingDocumentInput } from "./working-document-restore";
import { loadEnvironmentNodeIntroductionIntent } from "./environment-node-introduction.repository.server";

export const restoreWorkingDocument = Effect.fn("EnvironmentDesign.restoreWorkingDocument")(
  function* (actor: Actor, input: RestoreWorkingDocumentInput) {
    yield* requireEnvironmentForActorById(actor, input);
    return yield* withMutationReceipt(Effect.gen(function* () {
      const document = yield* loadEnvironmentDocument(input.environmentId, true);
      yield* requireDocumentRevision(document, input.revision);
      let baseline: SavedEnvironmentIntent | null = null;
      if (input.snapshotSource?.kind === "introduction") {
        if (input.command.kind !== "node") return yield* new Conflict({ message: "An introduction can restore only its node." });
        baseline = yield* loadEnvironmentNodeIntroductionIntent({ environmentId: input.environmentId,
          nodeType: input.command.nodeType, nodeId: input.command.nodeId });
      } else if (input.snapshotSource) {
        let savedStateSnapshotId: string;
        if (input.snapshotSource.kind === "saved") savedStateSnapshotId = input.snapshotSource.environmentSavedStateSnapshotId;
        else {
          const { drizzle } = yield* Database;
          const [deployment] = yield* drizzle.select({ savedStateSnapshotId: environmentDeployment.savedStateSnapshotId })
            .from(environmentDeployment).where(and(eq(environmentDeployment.id, input.snapshotSource.environmentDeploymentId), eq(environmentDeployment.environmentId, input.environmentId)));
          if (!deployment) return yield* new NotFound({ message: "Deployment baseline not found." });
          savedStateSnapshotId = deployment.savedStateSnapshotId;
        }
        const saved = yield* loadEnvironmentSavedIntentById({ environmentId: input.environmentId, savedStateSnapshotId });
        if (!saved) return yield* new NotFound({ message: "Saved baseline not found." });
        baseline = saved.intent;
      }
      const command = input.command;
      const next = yield* Effect.try({
        try: () => command.kind === "all" ? baseline ?? emptyEnvironmentIntent(document.namespace)
          : restoreEnvironmentNode(document.intent, baseline, { nodeType: command.nodeType, nodeId: command.nodeId }, command.path),
        catch: () => new Conflict({ message: "Discard would leave invalid Environment relationships." }),
      });
      return yield* writeEnvironmentDocument(document, next);
    }));
  },
);
