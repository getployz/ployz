import {
  decodeStrict,
  strictParseOptions,
  type DeepMutable,
} from "#/modules/environment-design/schema";
import { Effect, Schema } from "effect";
import type { EnvironmentResourceNodeType } from "#/modules/environment-design/environment-resource-node";
import { getServiceDeploymentDiffRows } from "#/modules/services/service-deployment-diff/fields";
import { discardServiceDeploymentDiffPath } from "#/modules/services/service-deployment-diff/mutations";
import {
  canonicalizeSavedEnvironmentIntent,
  savedEnvironmentIntentSchema,
  savedServiceIntentConfigSchema,
  type SavedEnvironmentIntent,
} from "#/modules/environment-design/saved-intent";
import { serviceDeploymentConfigSchema } from "#/modules/environment-design/services";
import { Conflict, Validation } from "#/server/public-error";

type SavedIntentNodeIdentity = {
  nodeType: "service" | EnvironmentResourceNodeType;
  nodeId: string;
};

/** Replaces one owner and validates the complete graph before it can persist. */
export const replaceSavedEnvironmentIntentNode = Effect.fn(
  "EnvironmentDesign.replaceSavedEnvironmentIntentNode",
)(function* (input: {
  current: SavedEnvironmentIntent;
  baseline: SavedEnvironmentIntent | null;
  node: SavedIntentNodeIdentity;
}) {
  const next: DeepMutable<SavedEnvironmentIntent> = structuredClone(
    input.current,
  );
  const baseline = input.baseline;

  switch (input.node.nodeType) {
  case "service": {
    next.services = next.services.filter(
      (service) => service.id !== input.node.nodeId,
    );
    const replacement = baseline?.services.find(
      (service) => service.id === input.node.nodeId,
    );
    if (replacement) next.services.push(structuredClone(replacement));
    break;
  }
  case "variable_group": {
    const current = next.variableGroups.find(
      (group) => group.resourceId === input.node.nodeId,
    );
    next.variableGroups = next.variableGroups.filter(
      (group) => group.resourceId !== input.node.nodeId,
    );
    const replacement = baseline?.variableGroups.find(
      (group) => group.resourceId === input.node.nodeId,
    );
    if (replacement) {
      next.variableGroups.push(structuredClone(replacement));
      if (!current && baseline) {
        for (const service of next.services) {
          const baselineService = baseline.services.find(
            (candidate) => candidate.id === service.id,
          );
          const baselineAttachment =
            baselineService?.variableGroupAttachments.find(
              (attachment) =>
                attachment.variableGroupId === replacement.variableGroupId,
            );
          if (
            baselineAttachment &&
            !service.variableGroupAttachments.some(
              (attachment) =>
                attachment.variableGroupId === replacement.variableGroupId,
            )
          ) {
            service.variableGroupAttachments.push(
              structuredClone(baselineAttachment),
            );
          }
        }
      } else if (
        current &&
        current.variableGroupId !== replacement.variableGroupId
      ) {
        for (const service of next.services) {
          service.variableGroupAttachments =
            service.variableGroupAttachments.map((attachment) =>
              attachment.variableGroupId === current.variableGroupId
                ? {
                    ...attachment,
                    variableGroupId: replacement.variableGroupId,
                  }
                : attachment,
            );
        }
      }
    } else if (current) {
      for (const service of next.services) {
        service.variableGroupAttachments =
          service.variableGroupAttachments.filter(
            (attachment) =>
              attachment.variableGroupId !== current.variableGroupId,
          );
      }
    }
    break;
  }
  case "volume": {
    const current = next.volumes.find(
      (volume) => volume.resourceId === input.node.nodeId,
    );
    next.volumes = next.volumes.filter(
      (volume) => volume.resourceId !== input.node.nodeId,
    );
    const replacement = baseline?.volumes.find(
      (volume) => volume.resourceId === input.node.nodeId,
    );
    if (replacement) {
      next.volumes.push(structuredClone(replacement));
      if (!current && baseline) {
        for (const service of next.services) {
          const baselineService = baseline.services.find(
            (candidate) => candidate.id === service.id,
          );
          const baselineAttachment = baselineService?.volumeAttachments.find(
            (attachment) =>
              attachment.volumeResourceId === replacement.resourceId,
          );
          if (
            baselineAttachment &&
            !service.volumeAttachments.some(
              (attachment) =>
                attachment.volumeResourceId === replacement.resourceId,
            )
          ) {
            service.volumeAttachments.push(structuredClone(baselineAttachment));
          }
        }
      }
    } else {
      for (const service of next.services) {
        service.volumeAttachments = service.volumeAttachments.filter(
          (attachment) => attachment.volumeResourceId !== input.node.nodeId,
        );
      }
    }
    break;
  }
  default: {
    const exhaustive: never = input.node.nodeType;
    return exhaustive;
  }
  }

  const parsed = yield* Schema.decodeUnknownEffect(savedEnvironmentIntentSchema)(
    next,
    strictParseOptions,
  ).pipe(
    Effect.mapError(
      () =>
        new Conflict({
          message: "Discard would leave invalid Saved Environment relationships.",
        }),
    ),
  );
  return canonicalizeSavedEnvironmentIntent(parsed);
});

export const discardSavedServiceIntentSetting = Effect.fn(
  "EnvironmentDesign.discardSavedServiceIntentSetting",
)(function* (input: {
  current: SavedEnvironmentIntent;
  baseline: SavedEnvironmentIntent;
  serviceId: string;
  path: string;
}) {
  const next: DeepMutable<SavedEnvironmentIntent> = structuredClone(
    input.current,
  );
  const currentService = next.services.find(
    (service) => service.id === input.serviceId,
  );
  const baselineService = input.baseline.services.find(
    (service) => service.id === input.serviceId,
  );
  if (!currentService || !baselineService) {
    return yield* new Validation({
      field: "change",
      message: "The pending Service setting no longer exists.",
    });
  }

  const draft = decodeStrict(serviceDeploymentConfigSchema, {
    ...currentService.config,
    env: {},
    mounts: [],
  });
  const baseline = decodeStrict(serviceDeploymentConfigSchema, {
    ...baselineService.config,
    env: {},
    mounts: [],
  });
  const row = getServiceDeploymentDiffRows({
    serviceId: input.serviceId,
    current: draft,
    baseline,
  }).find((candidate) => candidate.path === input.path && candidate.canDiscard);
  if (!row) {
    return yield* new Validation({
      field: "change.setting",
      message: "The requested Service setting cannot be discarded.",
    });
  }
  discardServiceDeploymentDiffPath({ draft, baseline, path: row.path });
  const { env: _env, mounts: _mounts, ...authoredConfig } = draft;
  void _env;
  void _mounts;
  currentService.config = decodeStrict(
    savedServiceIntentConfigSchema,
    authoredConfig,
  );
  const parsed = yield* Schema.decodeUnknownEffect(savedEnvironmentIntentSchema)(
    next,
    strictParseOptions,
  ).pipe(
    Effect.mapError(
      () =>
        new Conflict({
          message: "Discard would leave invalid Saved Environment relationships.",
        }),
    ),
  );
  return canonicalizeSavedEnvironmentIntent(parsed);
});
