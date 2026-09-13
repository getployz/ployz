import { restoreEnvironmentNode } from "@ployz/sdk/config";
import { Effect } from "effect";
import type { EnvironmentResourceNodeType } from "#/modules/environment-design/environment-resource-node";
import type { SavedEnvironmentIntent } from "#/modules/environment-design/saved-intent";
import { Conflict, Validation } from "#/server/public-error";

type SavedIntentNodeIdentity = {
  nodeType: "service" | EnvironmentResourceNodeType;
  nodeId: string;
};

export const replaceSavedEnvironmentIntentNode = Effect.fn(
  "EnvironmentDesign.replaceSavedEnvironmentIntentNode",
)((input: {
  current: SavedEnvironmentIntent;
  baseline: SavedEnvironmentIntent | null;
  node: SavedIntentNodeIdentity;
}) => Effect.try({
  try: () => restoreEnvironmentNode(input.current, input.baseline, input.node),
  catch: () => new Conflict({
    message: "Discard would leave invalid Saved Environment relationships.",
  }),
}));

export const discardSavedServiceIntentSetting = Effect.fn(
  "EnvironmentDesign.discardSavedServiceIntentSetting",
)((input: {
  current: SavedEnvironmentIntent;
  baseline: SavedEnvironmentIntent;
  serviceId: string;
  path: string;
}) => Effect.try({
  try: () => restoreEnvironmentNode(input.current, input.baseline, {
    nodeType: "service", nodeId: input.serviceId,
  }, input.path),
  catch: () => new Validation({
    field: "change.setting",
    message: "The requested Service setting cannot be discarded.",
  }),
}));
