import type {
  ServiceDeploymentConfig,
  ServiceDeploymentFieldSelection,
} from "#/modules/environment-design/services";
import {
  getPathValue,
  type ServiceDeploymentDiffPath,
} from "#/modules/services/service-deployment-diff/fields";
import { setDraftValueAtPath } from "#/utils/schema-path";

/**
 * Revert behavior is path-based via `setDraftValueAtPath`, so new fields do not
 * need a manual discard switch. The only special case is when the current and
 * baseline source variants differ, where discarding any `source.*` field falls
 * back to replacing the full source object.
 */
export function discardServiceDeploymentDiffPath(input: {
  draft: ServiceDeploymentFieldSelection;
  baseline: ServiceDeploymentConfig;
  path: ServiceDeploymentDiffPath;
}) {
  if (input.path.startsWith("routes.")) {
    const routeId = input.path.slice("routes.".length);
    const baselineRoute = input.baseline.routes.find(
      (route) => route.id === routeId,
    );
    input.draft.routes = baselineRoute
      ? [
          ...(input.draft.routes ?? []).filter((route) => route.id !== routeId),
          baselineRoute,
        ]
      : (input.draft.routes ?? []).filter((route) => route.id !== routeId);
    return;
  }

  if (input.path === "source") {
    input.draft.source = input.baseline.source;
    return;
  }

  if (
    input.path === "source.repository" &&
    input.draft.source.type === "git" &&
    input.baseline.source.type === "git"
  ) {
    input.draft.source.repository = input.baseline.source.repository;
    input.draft.source.repositoryId = input.baseline.source.repositoryId;
    input.draft.source.installationId = input.baseline.source.installationId;
    return;
  }

  if (
    input.path.startsWith("source.") &&
    input.draft.source.type !== input.baseline.source.type
  ) {
    input.draft.source = input.baseline.source;
    return;
  }

  const baselineValue = getPathValue(input.baseline, input.path);

  if (baselineValue === undefined) {
    return;
  }

  // SAFETY: draft/path/value are erased at the revert boundary; setDraftValueAtPath cannot prove DeepPath.
  setDraftValueAtPath(
    input.draft as never,
    input.path as never,
    baselineValue as never,
  );
}
