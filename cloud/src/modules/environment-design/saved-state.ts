import { Schema } from "effect";
import { Uuid } from "./schema";

const environmentNodeTypeSchema = Schema.Literals([
  "service",
  "variable_group",
  "volume",
]);

export const environmentSavedStateBasisSchema = Schema.Union([
  Schema.Struct({ kind: Schema.Literal("no_saved_state") }),
  Schema.Struct({
    kind: Schema.Literal("saved_revision"),
    savedStateSnapshotId: Uuid,
  }),
]);

export type EnvironmentSavedStateBasis =
  typeof environmentSavedStateBasisSchema.Type;

export const environmentSavedStateDiscardOperationSchema = Schema.Union([
  Schema.Struct({
    kind: Schema.Literal("node"),
    nodeType: environmentNodeTypeSchema,
    nodeId: Uuid,
  }),
  Schema.Struct({
    kind: Schema.Literal("setting"),
    nodeType: Schema.Literal("service"),
    nodeId: Uuid,
    setting: Schema.String.check(Schema.isNonEmpty()),
  }),
]);

export type EnvironmentSavedStateDiscardOperation =
  typeof environmentSavedStateDiscardOperationSchema.Type;

const discardOperationsSchema = Schema.mutable(
  Schema.Array(environmentSavedStateDiscardOperationSchema),
).check(
  Schema.isMinLength(1),
  Schema.makeFilter((operations) => {
    const identities = new Set<string>();
    const issues: Schema.FilterIssue[] = [];
    for (const [index, operation] of operations.entries()) {
      const identity =
        operation.kind === "node"
          ? `${operation.nodeType}:${operation.nodeId}:node`
          : `${operation.nodeType}:${operation.nodeId}:${operation.setting}`;
      if (identities.has(identity)) {
        issues.push({
          path: [index],
          issue: "A Saved change can be discarded only once per command.",
        });
      }
      identities.add(identity);
    }
    return issues;
  }),
);

export const environmentSavedStateDiscardCommandSchema = Schema.Struct({
  kind: Schema.Literal("discard"),
  basis: Schema.Struct({
    kind: Schema.Literal("saved_revision"),
    savedStateSnapshotId: Uuid,
  }),
  operations: discardOperationsSchema,
});

export type EnvironmentSavedStateDiscardCommand =
  typeof environmentSavedStateDiscardCommandSchema.Type;

export function environmentSavedStateBasisMatches(
  basis: EnvironmentSavedStateBasis,
  latestSavedStateSnapshotId: string | null,
) {
  return basis.kind === "no_saved_state"
    ? latestSavedStateSnapshotId === null
    : basis.savedStateSnapshotId === latestSavedStateSnapshotId;
}
