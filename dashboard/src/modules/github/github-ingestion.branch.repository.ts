import { strictParseOptions } from "#/modules/environment-design/schema";
import { and, eq, sql } from "drizzle-orm";
import { Cause, Effect, Exit, Option, Result as EffectResult, Schema } from "effect";
import {
  githubBranchProjection as schemaGithubBranchProjection,
  githubEnvironmentTrigger as schemaGithubEnvironmentTrigger,
} from "#/modules/github/tables";
import {
  isValidGithubBranchRef,
  isValidGithubEnvironmentTriggerSelection,
  isValidGithubExactSha,
  isValidGithubId,
  isValidGithubServiceCandidate,
  type GithubBranchCursor,
  type GithubServiceCandidate,
} from "#/modules/github/github-ingestion.contracts";
import { completeGithubDelivery } from "#/modules/github/github-ingestion.delivery.repository";
import {
  githubBranchEvaluationOutcome,
  selectGithubEnvironmentTriggers,
} from "#/modules/github/github-branch-evaluation";
import { withGithubTransaction } from "#/modules/github/github-ingestion.transaction";
import type {
  ApplyGithubBranchEvaluationInput,
  GithubBranchIdentity,
} from "#/modules/github/github-ingestion.repository.types";
import { Database, sqlErrorFrom } from "#/server/database.server";
import {
  GithubIngestionRepositoryError,
  repositoryError,
} from "#/modules/github/github-ingestion.repository.types";
import {
  admitEnvironmentDeployment,
  loadLatestSavedDeploymentTarget,
} from "#/modules/deployments/admission.server";
import { dispatchEnvironmentDeployment } from "#/modules/deployments/dispatch.server";
import {
  listLatestEnvironmentSavedStates,
} from "#/modules/environment-design/saved-state-repository.server";
import {
  serviceDeploymentConfigSchema,
  type ServiceDeploymentConfig,
} from "#/modules/environment-design/services";

function validIdentity(input: GithubBranchIdentity) {
  return (
    isValidGithubId(input.installationId) &&
    isValidGithubId(input.repositoryId) &&
    isValidGithubBranchRef(input.ref)
  );
}

function decodeCursor(
  row: typeof schemaGithubBranchProjection.$inferSelect,
): GithubBranchCursor | null {
  if (
    !Number.isSafeInteger(row.lastReceiptSequence) ||
    row.lastReceiptSequence < 1 ||
    !Number.isSafeInteger(row.evaluationRevision) ||
    row.evaluationRevision < 1
  ) {
    return null;
  }
  if (
    row.state === "active" &&
    row.evaluatedHeadSha &&
    isValidGithubExactSha(row.evaluatedHeadSha) &&
    row.evaluationReason !== "branch_deleted"
  ) {
    return {
      state: "active",
      headSha: row.evaluatedHeadSha,
      evaluationReason: row.evaluationReason,
      evaluationRevision: row.evaluationRevision,
      lastDeliveryId: row.lastDeliveryId,
      lastReceiptSequence: row.lastReceiptSequence,
    };
  }
  if (
    row.state === "deleted" &&
    row.evaluatedHeadSha === null &&
    row.evaluationReason === "branch_deleted"
  ) {
    return {
      state: "deleted",
      evaluationReason: "branch_deleted",
      evaluationRevision: row.evaluationRevision,
      lastDeliveryId: row.lastDeliveryId,
      lastReceiptSequence: row.lastReceiptSequence,
    };
  }
  return null;
}

function cursorsEqual(
  left: GithubBranchCursor | null,
  right: GithubBranchCursor | null,
) {
  if (!left || !right) return left === right;
  return (
    left.state === right.state &&
    left.evaluationReason === right.evaluationReason &&
    left.evaluationRevision === right.evaluationRevision &&
    left.lastDeliveryId === right.lastDeliveryId &&
    left.lastReceiptSequence === right.lastReceiptSequence &&
    (left.state === "deleted" ||
      (right.state === "active" && left.headSha === right.headSha))
  );
}

function decodeCandidate(
  row: {
    id: string;
    environmentId: string;
    config: ServiceDeploymentConfig;
  },
  identity: GithubBranchIdentity,
): GithubServiceCandidate | null | undefined {
  const source = row.config.source;
  const branch = source.type === "git" ? source.branch : null;
  if (
    source.type !== "git" ||
    source.version !== 2 ||
    source.installationId !== identity.installationId ||
    source.repositoryId !== identity.repositoryId ||
    source.autoDeploy !== true ||
    branch?.type !== "connected" ||
    branch.name !== identity.ref.slice("refs/heads/".length)
  ) {
    return undefined;
  }
  const candidate = {
    serviceId: row.id,
    environmentId: row.environmentId,
    watchPaths: row.config.build.watchPaths,
  };
  return isValidGithubServiceCandidate(candidate) ? candidate : null;
}

const savedStateCandidates = Effect.fn("Github.savedStateCandidates")(
  function* (
    saved: {
      environmentId: string;
      nodeSnapshots: Array<{
        nodeType: "service" | "variable_group" | "volume";
        nodeId: string;
        config: unknown;
      }>;
    },
    identity: GithubBranchIdentity,
  ) {
    const decoded = yield* Effect.forEach(
      saved.nodeSnapshots.filter((node) => node.nodeType === "service"),
      (node) =>
        Effect.gen(function* () {
          const config = yield* Schema.decodeUnknownEffect(
            serviceDeploymentConfigSchema,
          )(node.config, strictParseOptions).pipe(
            Effect.mapError(() =>
              repositoryError("invalid_stored_service", false),
            ),
          );
          const candidate = decodeCandidate(
            { id: node.nodeId, environmentId: saved.environmentId, config },
            identity,
          );
          if (candidate === null) {
            return yield* repositoryError("invalid_stored_service", false);
          }
          return candidate;
        }),
    );
    return decoded.filter((candidate) => candidate !== undefined);
  },
);

function mapSavedStateError(cause: unknown) {
  if (cause instanceof GithubIngestionRepositoryError) return cause;
  const sqlError = sqlErrorFrom(cause);
  if (sqlError !== undefined) return sqlError;
  return repositoryError("invalid_stored_service", false);
}

function isSqlFailure(cause: unknown) {
  return sqlErrorFrom(cause) !== undefined;
}

const compileLatestGithubTargets = Effect.fn("Github.compileLatestTargets")(
  function* (
    input: Extract<ApplyGithubBranchEvaluationInput["plan"], { kind: "active" }> &
      GithubBranchIdentity & {
        triggerOrigin: {
          origin: "github";
          deliveryId: string;
          branchEvaluationRevision: number;
          installationId: number;
          repositoryId: number;
        };
      },
  ) {
    const savedStates = yield* listLatestEnvironmentSavedStates();
    const candidates = (
      yield* Effect.forEach(savedStates, (saved) =>
        savedStateCandidates(saved, input),
      )
    ).flat();
    const selectedResult = selectGithubEnvironmentTriggers({
      candidates,
      selection: input.selection,
      changedPaths: input.changedPaths,
    });
    if (EffectResult.isFailure(selectedResult)) {
      return yield* repositoryError("invalid_stored_service", false);
    }
    const selected = selectedResult.success;
    const compiled = yield* Effect.forEach(selected, (trigger) =>
      Effect.gen(function* () {
        const target = yield* loadLatestSavedDeploymentTarget(
          trigger.environmentId,
        ).pipe(
          Effect.catchIf(
            (error) => !isSqlFailure(error),
            () => repositoryError("snapshot_not_admitted", false),
          ),
        );
        const environmentTarget = {
          ...target,
          environmentId: trigger.environmentId,
        };
        const latestCandidates = yield* savedStateCandidates(
          environmentTarget,
          input,
        );
        const latestSelectionResult = selectGithubEnvironmentTriggers({
          candidates: latestCandidates,
          selection: input.selection,
          changedPaths: input.changedPaths,
        });
        if (EffectResult.isFailure(latestSelectionResult)) {
          return yield* repositoryError("invalid_stored_service", false);
        }
        const latestTrigger = latestSelectionResult.success.find(
          ({ environmentId }) => environmentId === trigger.environmentId,
        );
        return latestTrigger
          ? { target: environmentTarget, trigger: latestTrigger }
          : null;
      }),
    );
    return compiled.filter((row) => row !== null);
  },
  Effect.mapError(mapSavedStateError),
);

export const loadGithubBranchCursor = Effect.fn("Github.loadBranchCursor")(
  function* (input: GithubBranchIdentity) {
    if (!validIdentity(input)) {
      return yield* repositoryError("invalid_input", false);
    }
    const { drizzle } = yield* Database;
    const [row] = yield* drizzle
      .select()
      .from(schemaGithubBranchProjection)
      .where(
        and(
          eq(
            schemaGithubBranchProjection.installationId,
            input.installationId,
          ),
          eq(schemaGithubBranchProjection.repositoryId, input.repositoryId),
          eq(schemaGithubBranchProjection.ref, input.ref),
        ),
      )
      .limit(1);
    if (!row) return null;
    const cursor = decodeCursor(row);
    return cursor
      ? cursor
      : yield* repositoryError("database_error", false);
  },
);

export const listGithubServiceCandidates = Effect.fn(
  "Github.listServiceCandidates",
)(
  function* (input: GithubBranchIdentity) {
    if (!validIdentity(input)) {
      return yield* repositoryError("invalid_input", false);
    }
    const savedStates = yield* listLatestEnvironmentSavedStates();
    const candidates = (
      yield* Effect.forEach(savedStates, (saved) =>
        savedStateCandidates(saved, input),
      )
    ).flat();
    return candidates.sort((left, right) =>
      `${left.environmentId}:${left.serviceId}`.localeCompare(
        `${right.environmentId}:${right.serviceId}`,
      ),
    );
  },
  Effect.mapError(mapSavedStateError),
);

function validPlan(input: ApplyGithubBranchEvaluationInput) {
  if (input.plan.kind === "stale") return true;
  const expectedRevision = (input.expectedCursor?.evaluationRevision ?? 0) + 1;
  if (input.plan.branch.evaluationRevision !== expectedRevision) return false;
  if (input.plan.kind === "deleted") return input.plan.triggers.length === 0;
  if (!isValidGithubExactSha(input.plan.branch.headSha)) {
    return false;
  }
  const environments = new Set<string>();
  for (const trigger of input.plan.triggers) {
    if (
      !/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(
        trigger.environmentId,
      ) ||
      environments.has(trigger.environmentId) ||
      trigger.serviceIds.length === 0 ||
      trigger.serviceIds.some(
        (id) =>
          !/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(
            id,
          ),
      ) ||
      new Set(trigger.serviceIds).size !== trigger.serviceIds.length ||
      [...trigger.serviceIds]
        .sort()
        .some((serviceId, index) => serviceId !== trigger.serviceIds[index]) ||
      !isValidGithubEnvironmentTriggerSelection(trigger.selection)
    ) {
      return false;
    }
    environments.add(trigger.environmentId);
  }
  return true;
}

const admitActiveGithubDeployments = Effect.fn(
  "Github.admitActiveGithubDeployments",
)(function* (input: ApplyGithubBranchEvaluationInput & {
  plan: Extract<ApplyGithubBranchEvaluationInput["plan"], { kind: "active" }>;
}) {
  const { drizzle } = yield* Database;
  const headSha = input.plan.branch.headSha;
  const branch = input.plan.branch;
  const triggerOrigin = {
    origin: "github" as const,
    deliveryId: input.deliveryId,
    branchEvaluationRevision: branch.evaluationRevision,
    installationId: input.installationId,
    repositoryId: input.repositoryId,
  };
  const compiled = yield* compileLatestGithubTargets({
    ...input.plan,
    ...input,
    triggerOrigin,
  });
  const admitted = yield* Effect.forEach(
    compiled,
    ({ target, trigger }) =>
      Effect.gen(function* () {
        const [inserted] = yield* drizzle
          .insert(schemaGithubEnvironmentTrigger)
          .values({
            installationId: input.installationId,
            repositoryId: input.repositoryId,
            ref: input.ref,
            headSha,
            environmentId: trigger.environmentId,
            serviceIds: [...trigger.serviceIds],
            selectionMode: trigger.selection.mode,
            reason: trigger.selection.reason,
            sourceDeliveryId: input.deliveryId,
            sourceReceiptSequence: input.receiptSequence,
            branchEvaluationRevision: branch.evaluationRevision,
            triggerRevision: branch.evaluationRevision,
          })
          .onConflictDoNothing()
          .returning({ id: schemaGithubEnvironmentTrigger.id });
        if (!inserted) return null;
        const deployment = yield* admitEnvironmentDeployment({
          environmentId: target.environmentId,
          savedStateSnapshotId: target.savedStateSnapshotId,
          triggerOrigin,
          message: null,
        }).pipe(
          Effect.catchIf(
            (error) => !isSqlFailure(error),
            () => repositoryError("snapshot_not_admitted", false),
          ),
        );
        return {
          environmentDeploymentId: deployment.id,
          environmentId: trigger.environmentId,
        };
      }),
  );
  return admitted.filter((row) => row !== null);
});

export const applyGithubBranchEvaluation = Effect.fn(
  "Github.applyBranchEvaluation",
)(function* (input: ApplyGithubBranchEvaluationInput) {
  if (
    !validIdentity(input) ||
    !isValidGithubId(input.receiptSequence) ||
    !input.processingRunId ||
    !validPlan(input)
  ) {
    return yield* repositoryError("invalid_input", false);
  }
  const { applied, deployments } = yield* withGithubTransaction(
      Effect.gen(function* () {
        const { drizzle } = yield* Database;
        const authorityKey = `${input.installationId}:${input.repositoryId}:${input.ref}`;
        yield* drizzle.execute(
          sql`select pg_advisory_xact_lock(hashtextextended(${authorityKey}, 110))`,
        );
        const [row] = yield* drizzle
          .select()
          .from(schemaGithubBranchProjection)
          .where(
            and(
              eq(
                schemaGithubBranchProjection.installationId,
                input.installationId,
              ),
              eq(schemaGithubBranchProjection.repositoryId, input.repositoryId),
              eq(schemaGithubBranchProjection.ref, input.ref),
            ),
          )
          .for("update");
        const cursor = row ? decodeCursor(row) : null;
        if (row && !cursor) {
          return yield* repositoryError("database_error", false);
        }
        if (!cursorsEqual(cursor, input.expectedCursor)) {
          return yield* repositoryError("cursor_conflict", true);
        }
        if (input.plan.kind === "stale") {
          yield* completeGithubDelivery(
            {
              deliveryId: input.deliveryId,
              receiptSequence: input.receiptSequence,
              processingRunId: input.processingRunId,
              identity: { ...input, eventKind: "push" },
            },
            input.plan.outcome,
          );
          return {
            applied: {
              disposition: "stale" as const,
              triggersCreated: 0,
              cursor,
            },
            deployments: [],
          };
        }

        const branch = input.plan.branch;
        const projection = {
          installationId: input.installationId,
          repositoryId: input.repositoryId,
          ref: input.ref,
          state: branch.state,
          evaluatedHeadSha: branch.state === "active" ? branch.headSha : null,
          evaluationReason: branch.evaluationReason,
          evaluationRevision: branch.evaluationRevision,
          lastDeliveryId: input.deliveryId,
          lastReceiptSequence: input.receiptSequence,
          updatedAt: new Date(),
        } as const;
        const projectedRows = row
          ? yield* drizzle
              .update(schemaGithubBranchProjection)
              .set(projection)
              .where(
                and(
                  eq(
                    schemaGithubBranchProjection.installationId,
                    input.installationId,
                  ),
                  eq(
                    schemaGithubBranchProjection.repositoryId,
                    input.repositoryId,
                  ),
                  eq(schemaGithubBranchProjection.ref, input.ref),
                  eq(
                    schemaGithubBranchProjection.evaluationRevision,
                    input.expectedCursor?.evaluationRevision ?? 0,
                  ),
                ),
              )
              .returning()
          : yield* drizzle
              .insert(schemaGithubBranchProjection)
              .values(projection)
              .onConflictDoNothing()
              .returning();
        const projected = projectedRows[0];
        const nextCursor = projected ? decodeCursor(projected) : null;
        if (!nextCursor) {
          return yield* repositoryError("cursor_conflict", true);
        }

        const deployments =
          input.plan.kind === "active"
            ? yield* admitActiveGithubDeployments({
                ...input,
                plan: input.plan,
              })
            : [];
        yield* completeGithubDelivery(
          {
            deliveryId: input.deliveryId,
            receiptSequence: input.receiptSequence,
            processingRunId: input.processingRunId,
            identity: { ...input, eventKind: "push" },
          },
          input.plan.kind === "active"
            ? githubBranchEvaluationOutcome(
                input.plan.selection,
                deployments.length,
              )
            : input.plan.outcome,
        );
        return {
          applied: {
            disposition: "applied" as const,
            triggersCreated: deployments.length,
            cursor: nextCursor,
          },
          deployments,
        };
      }),
      "read committed",
    );
    const dispatched = yield* Effect.all(
      deployments.map((deployment) =>
        Effect.exit(dispatchEnvironmentDeployment(deployment)),
      ),
      { concurrency: "unbounded" },
    );
    for (const result of dispatched) {
      if (Exit.isSuccess(result)) continue;
      const failure = Cause.findErrorOption(result.cause);
      if (Option.isNone(failure)) continue;
      const sqlError = sqlErrorFrom(failure.value);
      if (sqlError !== undefined) {
        return yield* Effect.fail(sqlError);
      }
    }
    return applied;
});
