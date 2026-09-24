import "@tanstack/react-start/server-only";
import { createRequire } from "node:module";
import type {
  Client,
  LogOptions, LogEvent, LogHistoryOptions, LogHistoryPage,
  EnrollmentAssignment,
  EnrollmentSnapshot,
  ClusterTeardown,
  ConnectOptions,
  Connection,
  DataLossConfirmation,
  DeployOutcome,
  DeployEvent,
  DeployIntent,
  ExecutionError,
  MachineDetails,
  MachineTarget,
  ObservedDataLoss,
  PreparedDeploy,
  PreparationInput,
  PreparationEvent,
  ProjectName,
  RemoveVolumesRequest,
  RuntimeWatchView,
  WatchOptions,
  VolumeRemoval,
} from "@ployz/sdk";
import type * as PloyzSdk from "@ployz/sdk";
import { Context, Data, Effect, Exit, Layer, Option, Schema, type Scope } from "effect";
import type { JsonValue } from "#/db/tables";
import { projectJsonValue } from "#/lib/json";
import { MissingDataLossIdentities } from "#/modules/runtime/data-loss-confirm";
import { dataLossIdentitySchema } from "#/modules/runtime/data-loss-identity";
import { RuntimeConnectionFailure } from "#/modules/runtime/runtime-connection-errors";

// SAFETY: the package exports this named CommonJS SDK surface at runtime.
const { connect: connectSdk } = createRequire(import.meta.url)("@ployz/sdk") as Pick<typeof PloyzSdk, "connect">;

export class PloyzProviderError extends Data.TaggedError(
  "PloyzProviderError",
)<{
  readonly operation: string;
  readonly cause: unknown;
}> {}

export class PloyzPreparationError extends Data.TaggedError("PloyzPreparationError")<{
  readonly failureCode: "sdk_preparation_failed" | "sdk_preparation_unknown" | "sdk_preparation_cancelled";
  readonly message: string;
  readonly cause?: unknown;
  readonly stage?: string | undefined;
  readonly work?: Record<string, string> | undefined;
}> { readonly retriable = false as const; }
class PreparationProgressError extends Data.TaggedError("PreparationProgressError")<{
  readonly cause: unknown;
}> {}

const preparationFailureSchema = Schema.Struct({ details: Schema.Struct({ preparation: Schema.Struct({
  kind: Schema.Literals(["failed", "unknown", "cancelled"]),
  stage: Schema.optional(Schema.Literals(["Selection", "Observation", "Admission", "Queued", "Upload", "Preparation", "Building", "Output", "Cleanup"])),
  message: Schema.optional(Schema.String),
  rejections: Schema.optional(Schema.NullOr(Schema.Struct({
    "membership unavailable": Schema.optional(Schema.Number),
    "builds disabled": Schema.optional(Schema.Number),
    "capability unverified": Schema.optional(Schema.Number),
  }))),
  work: Schema.optional(Schema.Record(Schema.String, Schema.Union([
    Schema.Literals(["Unattempted", "Unknown", "Validated", "Published"]), Schema.Struct({ Image: Schema.Unknown }),
  ]))),
}) }) });

export type PloyzSdkError =
  | PloyzProviderError
  | PloyzPreparationError
  | MissingDataLossIdentities;

export type PloyzPreparedDeploy = Omit<PreparedDeploy, "confirm"> & {
  readonly confirm: (onEvent?: (event: DeployEvent) => Promise<void>, cancellation?: AbortSignal, deploymentId?: string) => Effect.Effect<unknown, PloyzSdkError>;
};

export interface PloyzSession {
  readonly logs: (options: LogOptions) => Effect.Effect<AsyncIterable<LogEvent>, PloyzSdkError>;
  readonly logHistory: (options: LogHistoryOptions) => Effect.Effect<LogHistoryPage, PloyzSdkError>;
  readonly inspect: () => Effect.Effect<MachineDetails, PloyzSdkError>;
  readonly clearManagementClient: (label: string) => Effect.Effect<void, PloyzSdkError>;
  readonly observeEnrollment: () => Effect.Effect<EnrollmentSnapshot, PloyzProviderError>;
  readonly register: (assignment: EnrollmentAssignment) => Effect.Effect<JsonValue, PloyzProviderError>;
  readonly removeMachine: (
    machine: MachineTarget,
    confirmDataLoss: DataLossConfirmation,
  ) => Effect.Effect<void, PloyzSdkError>;
  readonly dataLossIfMachineRemoved: (
    machine: MachineTarget,
  ) => Effect.Effect<ObservedDataLoss, PloyzSdkError>;
  readonly dataLossIfProjectDestroyed: (
    projectName: ProjectName,
    destroyVolumes?: boolean,
  ) => Effect.Effect<ObservedDataLoss, PloyzSdkError>;
  readonly destroyProject: (
    projectName: ProjectName,
    confirmDataLoss: DataLossConfirmation,
    destroyVolumes?: boolean,
  ) => Effect.Effect<DeployOutcome<ExecutionError>, PloyzSdkError>;
  readonly dataLossIfClusterDestroyed: () => Effect.Effect<
    ObservedDataLoss,
    PloyzSdkError
  >;
  readonly destroyCluster: (
    confirmDataLoss: DataLossConfirmation,
  ) => Effect.Effect<ClusterTeardown, PloyzSdkError>;
  readonly removeVolumes: (
    request: RemoveVolumesRequest,
  ) => Effect.Effect<VolumeRemoval[], PloyzSdkError>;
  readonly prepare: (
    input: PreparationInput,
    onEvent: (event: PreparationEvent) => Promise<void>,
    cancellation: AbortSignal,
  ) => Effect.Effect<PloyzPreparedDeploy, PloyzSdkError, Scope.Scope>;
  readonly preview: (
    intent: DeployIntent,
  ) => Effect.Effect<PloyzPreparedDeploy, PloyzSdkError>;
  readonly watch: (
    options?: WatchOptions,
  ) => Effect.Effect<AsyncIterable<RuntimeWatchView>, RuntimeConnectionFailure>;
  readonly watchFirstFrame: (
    timeoutMs: number,
  ) => Effect.Effect<RuntimeWatchView, RuntimeConnectionFailure>;
}

type SharedConnectOptions = Omit<
  Extract<ConnectOptions, { readonly connections: readonly Connection[] }>,
  "signal"
>;

type PloyzBindings = {
  readonly connect: (options: ConnectOptions) => Promise<Client>;
};

export interface PloyzService {
  readonly connect: (
    options: SharedConnectOptions,
  ) => Effect.Effect<PloyzSession, PloyzProviderError, Scope.Scope>;
}

export class Ployz extends Context.Service<Ployz, PloyzService>()(
  "ployz/Ployz",
) {}

const UnconfirmedDataLossDetails = Schema.Struct({
  missing: Schema.Array(dataLossIdentitySchema),
});
const UnconfirmedDataLossError = Schema.Struct({
  code: Schema.Literal("invalid_argument"),
  message: Schema.String,
  details: UnconfirmedDataLossDetails,
});

function missingDataLossFromSdkError(cause: unknown) {
  const decoded = Schema.decodeUnknownOption(UnconfirmedDataLossError)(cause);
  if (
    Option.isNone(decoded) ||
    decoded.value.details.missing.length === 0
  ) return null;
  return new MissingDataLossIdentities(decoded.value.details.missing);
}

function safePreparationDiagnosis(message: string, secrets: readonly string[]) {
  let safe = message;
  for (const secret of secrets) if (secret.length > 0) safe = safe.replaceAll(secret, "[redacted]");
  return safe.replace(/ployz1:[^\s"'<>]+/g, "[redacted capability]")
    .replace(/https?:\/\/[^\s/@]+:[^\s/@]+@/g, "https://[redacted]@")
    .replace(/\b(token|password|secret|authorization|credential)[=:]\s*(?:Bearer\s+)?[^\s,;]+/gi, "$1=[redacted]")
    .split("").filter((character) => character === "\n" || character === "\t" || character.charCodeAt(0) >= 32 && character.charCodeAt(0) !== 127).join("")
    .slice(-2048);
}

function asSdkFailure(operation: string, cause: unknown, secrets: readonly string[] = []): PloyzSdkError {
  if (cause instanceof PloyzPreparationError) return cause;
  if (operation === "prepare") {
    const failure = Schema.decodeUnknownOption(preparationFailureSchema)(cause);
    if (Option.isSome(failure)) {
      const { kind, stage, work, message, rejections } = failure.value.details.preparation;
      const diagnosis = message ? safePreparationDiagnosis(message, secrets) : undefined;
      const selection = rejections ? Object.entries(rejections).map(([reason, count]) => `${count} ${reason}`).join("; ") : "";
      return new PloyzPreparationError({ failureCode: `sdk_preparation_${kind}`, stage,
        work: work ? Object.fromEntries(Object.entries(work).map(([target, evidence]) => [target, Schema.is(Schema.String)(evidence) ? evidence : "Image"])) : undefined,
        message: [kind === "unknown" ? "Remote work outcome is unknown." : kind === "cancelled" ? "Build cancelled." : `Preparation failed${stage ? ` during ${stage}` : ""}.`, diagnosis, selection].filter(Boolean).join(" "),
      });
    }
    if (Schema.is(Schema.Struct({ code: Schema.Literal("invalid_argument") }))(cause)) {
      return new PloyzPreparationError({ failureCode: "sdk_preparation_failed", message: "Preparation input is invalid; no build was started." });
    }
    return new PloyzPreparationError({ failureCode: "sdk_preparation_unknown", message: "Preparation ended without a confirmed result; remote work outcome is unknown.", cause });
  }
  const missingDataLoss = missingDataLossFromSdkError(cause);
  if (missingDataLoss !== null) return missingDataLoss;
  if (cause instanceof MissingDataLossIdentities) return cause;
  if (cause instanceof PloyzProviderError) return cause;
  return new PloyzProviderError({ operation, cause });
}

function sdkPromise<A>(operation: string, run: (signal: AbortSignal) => Promise<A>, secrets: readonly string[] = []) {
  return Effect.tryPromise({
    try: run,
    catch: (cause) => asSdkFailure(operation, cause, secrets),
  });
}

function wrapPrepared(prepared: PreparedDeploy): PloyzPreparedDeploy {
  return {
    ...prepared,
    confirm: (onEvent, cancellation, deploymentId) => Effect.scoped(Effect.gen(function* () {
      const running = yield* Effect.acquireRelease(
        Effect.try({ try: () => prepared.confirm({ signal: cancellation, deploymentId }), catch: (cause) => asSdkFailure("confirm", cause) }),
        (running, exit) => Effect.promise(async () => {
          if (Exit.isFailure(exit)) running.abort();
          await running.finished.catch(() => undefined);
        }),
      );
      const abort = () => running.abort();
      if (cancellation?.aborted) abort();
      else cancellation?.addEventListener("abort", abort, { once: true });
      yield* Effect.addFinalizer(() => Effect.sync(() => cancellation?.removeEventListener("abort", abort)));
      return yield* sdkPromise("confirm", async () => {
        let outcome: unknown;
        for await (const event of running) {
          await onEvent?.(event);
          if (event.type === "outcome") outcome = event.outcome;
        }
        if (outcome === undefined) {
          const finished = await running.finished;
          await onEvent?.({ type: "outcome", outcome: finished });
          outcome = finished;
        }
        return outcome;
      });
    })),
  };
}

function wrapClient(client: Client): PloyzSession {
  const watch = (options?: WatchOptions) =>
    Effect.try({
      try: () => client.runtime.watch(options),
      catch: (cause) => new RuntimeConnectionFailure({ cause }),
    });
  return {
    logs: (options) => Effect.try({ try: () => client.runtime.logs(options), catch: (cause) => asSdkFailure("logs", cause) }),
    logHistory: (options) => sdkPromise("log history", (signal) => client.runtime.logHistory({ ...options, signal })),
    inspect: () => sdkPromise("inspect", () => client.inspect()),
    clearManagementClient: (label) => sdkPromise("clear Management Client", () => client.clearManagementClient(label)),
    observeEnrollment: () => Effect.tryPromise({
      try: () => client.observeEnrollment(),
      catch: (cause) => new PloyzProviderError({ operation: "observe enrollment", cause }),
    }),
    register: (assignment) => Effect.tryPromise({
      try: async () => {
        const json = projectJsonValue(await client.register(assignment));
        if (json === undefined) throw new Error("Invalid registration response");
        return json;
      },
      catch: (cause) => new PloyzProviderError({ operation: "register", cause }),
    }),
    removeMachine: (machine, confirmDataLoss) =>
      sdkPromise("remove machine", () =>
        client.removeMachine(machine, confirmDataLoss).then(() => undefined),
      ),
    dataLossIfMachineRemoved: (machine) =>
      sdkPromise("load machine data loss", () =>
        client.dataLossIfMachineRemoved(machine),
      ),
    dataLossIfProjectDestroyed: (projectName, destroyVolumes) =>
      sdkPromise("load project data loss", () =>
        client.dataLossIfProjectDestroyed(projectName, destroyVolumes),
      ),
    destroyProject: (projectName, confirmDataLoss, destroyVolumes) =>
      sdkPromise("destroy project", () =>
        client.destroyProject(projectName, confirmDataLoss, destroyVolumes),
      ),
    dataLossIfClusterDestroyed: () =>
      sdkPromise("load cluster data loss", () =>
        client.dataLossIfClusterDestroyed(),
      ),
    destroyCluster: (confirmDataLoss) =>
      sdkPromise("destroy cluster", () =>
        client.destroyCluster(confirmDataLoss),
      ),
    removeVolumes: (request) =>
      sdkPromise("remove volumes", () => client.removeVolumes(request)),
    prepare: (input, onEvent, cancellation) => Effect.gen(function* () {
      const secrets = input.deployment.snapshots.flatMap((snapshot) => Object.values(snapshot.resolvedEnv ?? {}));
      const running = yield* Effect.acquireRelease(
        Effect.try({ try: () => client.prepare(input, { signal: cancellation }), catch: (cause) => asSdkFailure("prepare", cause, secrets) }),
        (running) => Effect.promise(async () => {
          running.abort();
          // Interruption also waits for native cleanup before releasing the attempt slot.
          await running.finished.then((prepared) => prepared.close(), () => undefined);
        }),
      );
      return yield* sdkPromise("prepare", async () => {
        try {
          for await (const event of running) {
            try { await onEvent(event); }
            catch (cause) { throw new PreparationProgressError({ cause }); }
          }
          return wrapPrepared(await running.finished);
        } catch (cause) {
          running.abort();
          const confirmed = await running.finished.then(async (prepared) => {
            await prepared.close();
            return true;
          }, (error) => {
            const failure = asSdkFailure("prepare", error, secrets);
            return failure instanceof PloyzPreparationError && failure.failureCode !== "sdk_preparation_unknown";
          });
          if (cause instanceof PreparationProgressError) {
            throw new PloyzPreparationError({
              failureCode: confirmed ? "sdk_preparation_failed" : "sdk_preparation_unknown",
              message: "Could not save build progress." + (confirmed ? "" : " Remote work outcome is unknown."),
              cause: cause.cause,
            });
          }
          throw cause;
        }
      }, secrets);
    }),
    preview: (intent) =>
      sdkPromise("preview", () => client.preview(intent)).pipe(
        Effect.map(wrapPrepared),
      ),
    watch,
    watchFirstFrame: (timeoutMs) =>
      Effect.tryPromise({
        try: async (signal) => {
          const frames = client.runtime.watch({ signal });
          for await (const frame of frames) return frame;
          throw new Error("Runtime watch ended before the first frame");
        },
        catch: (cause) => new RuntimeConnectionFailure({ cause }),
      }).pipe(
        Effect.timeoutOrElse({
          duration: timeoutMs,
          orElse: () =>
            Effect.fail(
              new RuntimeConnectionFailure({
                cause: new Error("Runtime watch timed out"),
              }),
            ),
        }),
      ),
  };
}

function closeSession(session: Client) {
  return Effect.tryPromise({
    try: () => session.close(),
    catch: (cause) => new PloyzProviderError({ operation: "close", cause }),
  }).pipe(
    Effect.tapError((error) =>
      Effect.logError("The Ployz session finalizer failed.", error),
    ),
    Effect.ignore,
  );
}

export function makePloyzLayer(bindings: PloyzBindings) {
  return Layer.succeed(Ployz, {
    connect: (options) =>
      Effect.gen(function* () {
        const controller = yield* Effect.acquireRelease(
          Effect.sync(() => new AbortController()),
          (controller) => Effect.sync(() => controller.abort()),
        );
        return yield* Effect.acquireRelease(
          Effect.tryPromise({
            try: (signal) => bindings.connect({
              ...options,
              signal: AbortSignal.any([signal, controller.signal]),
            }),
            catch: (cause) =>
              new PloyzProviderError({ operation: "connect", cause }),
          }),
          closeSession,
          { interruptible: true },
        ).pipe(Effect.map(wrapClient));
      }),
  });
}

export const PloyzLive = makePloyzLayer({
  connect: connectSdk,
});
