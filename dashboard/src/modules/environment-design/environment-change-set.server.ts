import "@tanstack/react-start/server-only";
import { compareServiceSettings, parseServiceConfig, restoreEnvironmentNode } from "@ployz/sdk/config";
import { Effect } from "effect";
import type { Actor } from "#/modules/identity/actor";
import { loadEnvironmentSnapshotProjection, type EnvironmentSnapshotProjection } from "#/modules/deployments/environment-state.repository.server";
import { lockEnvironmentDeploymentQueue } from "#/modules/deployments/queue-lock.server";
import { withMutationResult } from "#/server/mutation-result.server";
import { Conflict } from "#/server/public-error";
import { requireEnvironmentForActorById } from "./authoring-repository.server";
import { loadEnvironmentDocument, requireDocumentRevision, writeEnvironmentDocument } from "./working-state-repository.server";
import { loadEnvironmentSavedIntentById, loadLatestEnvironmentSavedState } from "./saved-state-repository.server";
import { publishEnvironmentSavedState } from "./saved-state-operations.server";
import { environmentSavedStateBasisMatches } from "./saved-state";
import { emptyEnvironmentIntent, type SavedEnvironmentIntent } from "./saved-intent";
import { loadEnvironmentNodeIntroductionIntent } from "./environment-node-introduction.repository.server";
import type { DiscardEnvironmentChangesInput } from "./working-document-restore";

/** Reconstruct authored Applied State from each node's confirmed Saved revision. */
const loadAppliedIntent = Effect.fn("EnvironmentDesign.loadAppliedIntent")(
  function* (environmentId: string, namespace: string, projection: EnvironmentSnapshotProjection) {
    const nodes = [...projection.appliedSavedNodeByKey.values()];
    const intents = new Map<string, SavedEnvironmentIntent>();
    for (const savedStateSnapshotId of new Set(nodes.map(node => node.sourceSavedStateSnapshotId))) {
      const saved = yield* loadEnvironmentSavedIntentById({ environmentId, savedStateSnapshotId });
      if (!saved) return yield* new Conflict({ message: "Applied State is missing its authored revision." });
      intents.set(savedStateSnapshotId, saved.intent);
    }
    const baseline = emptyEnvironmentIntent(namespace);
    for (const node of nodes) {
      const source = intents.get(node.sourceSavedStateSnapshotId);
      if (!source) return yield* new Conflict({ message: "Applied State is missing its authored revision." });
      if (node.nodeType === "service") {
        const value = source.services.find(value => value.id === node.nodeId);
        if (!value) return yield* new Conflict({ message: "Applied Service is missing." });
        baseline.services.push(value);
      } else if (node.nodeType === "variable_group") {
        const value = source.variableGroups.find(value => value.resourceId === node.nodeId);
        if (!value) return yield* new Conflict({ message: "Applied Variable Group is missing." });
        baseline.variableGroups.push(value);
      } else {
        const value = source.volumes.find(value => value.resourceId === node.nodeId);
        if (!value) return yield* new Conflict({ message: "Applied Volume is missing." });
        baseline.volumes.push(value);
      }
    }
    return baseline;
  },
);

/** One transaction restores the reviewed scope in Working and Saved State. */
export const discardEnvironmentChanges = Effect.fn("EnvironmentDesign.discardEnvironmentChanges")(
  function* (actor: Actor, input: DiscardEnvironmentChangesInput) {
    yield* requireEnvironmentForActorById(actor, input);
    return yield* withMutationResult(Effect.gen(function* () {
      yield* lockEnvironmentDeploymentQueue(input.environmentId);
      const document = yield* loadEnvironmentDocument(input.environmentId, true);
      yield* requireDocumentRevision(document, input.revision);
      const projection = yield* loadEnvironmentSnapshotProjection({ kind: "environment", environmentId: input.environmentId });
      const state = projection.explicitStates.find(state => state.environmentId === input.environmentId);
      const submitted = state?.deploymentEvidence;
      const baselineToken = (submitted ?? state?.applied)?.token ?? "applied:none";
      const latest = yield* loadLatestEnvironmentSavedState(input.environmentId);
      if (baselineToken !== input.baselineToken || !environmentSavedStateBasisMatches(input.savedStateBasis, latest?.id ?? null)) {
        return yield* new Conflict({ message: "Environment changes moved after this review. Review the latest changes and try again." });
      }
      let baseline: SavedEnvironmentIntent;
      if (submitted) {
        const saved = yield* loadEnvironmentSavedIntentById({
          environmentId: input.environmentId, savedStateSnapshotId: submitted.savedStateSnapshotId,
        });
        if (!saved) return yield* new Conflict({ message: "Submitted Saved State is missing." });
        baseline = saved.intent;
      } else {
        baseline = yield* loadAppliedIntent(input.environmentId, document.namespace, projection);
      }
      const command = input.command;
      let introduction = false;
      if (command.kind === "node" && command.path) {
        const exists = (nodes: Array<{ nodeType: string; nodeId: string; config: unknown }> = []) =>
          nodes.some(node => node.nodeType === command.nodeType && node.nodeId === command.nodeId && node.config !== null);
        if (!exists((submitted ?? state?.applied)?.nodes) && !exists(state?.saved?.nodes) && !exists(state?.applied.nodes)) {
          baseline = yield* loadEnvironmentNodeIntroductionIntent({
            environmentId: input.environmentId, nodeType: command.nodeType, nodeId: command.nodeId,
          });
          introduction = true;
        }
      }
      const restore = (current: SavedEnvironmentIntent) => Effect.try({
        try: () => command.kind === "all" ? baseline
          : restoreEnvironmentNode(current, baseline, command, command.path),
        catch: () => new Conflict({ message: "Discard would leave invalid Environment relationships." }),
      });
      const working = yield* restore(document.intent);
      const savedNode = command.kind === "node" ? state?.saved?.nodes.find(node =>
        node.nodeType === command.nodeType && node.nodeId === command.nodeId) : null;
      const baselineNode = command.kind === "node" ? (submitted ?? state?.applied)?.nodes.find(node =>
        node.nodeType === command.nodeType && node.nodeId === command.nodeId) : null;
      const savedNeedsRestore = command.kind === "all" || !command.path ||
        (savedNode?.config != null && baselineNode?.config != null &&
          compareServiceSettings(parseServiceConfig(savedNode.config), parseServiceConfig(baselineNode.config))
            .some(row => row.path === command.path && row.canRestore));
      // New-node field resets use its Introduction and do not publish it.
      if (latest && !introduction && savedNeedsRestore) {
        const saved = yield* restore(latest.intent);
        yield* publishEnvironmentSavedState({
          environmentId: input.environmentId, actorId: actor.userId,
          message: "Discard changes", basis: input.savedStateBasis, intent: saved,
          destructiveVolumeReviews: [], revisionPolicy: "reuse_latest_if_equivalent",
        });
      }
      return yield* writeEnvironmentDocument(document, working);
    }));
  },
);
