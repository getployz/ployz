import "@tanstack/react-start/server-only";
import { createRequire } from "node:module";
import type {
  Client,
  LogOptions, LogEvent, LogHistoryOptions, LogHistoryPage,
  EnrollmentAssignment,
  EnrollmentSnapshot,
  ConnectOptions,
  Connection,
  DataLossConfirmation,
  DeployOutcome,
  ExecutionError,
  LocalMachineRemoved,
  MachineDetails,
  MachineTarget,
  MachineUpdate,
  ObservedDataLoss,
  PublishCertificateMaterialRequest,
  RuntimeWatchView,
  WatchOptions,
} from "@ployz/sdk";
import type * as PloyzSdk from "@ployz/sdk";
import { Context, Data, Effect, Layer, Option, Schema, type Scope } from "effect";
import type { JsonValue } from "#/db/tables";
import { projectJsonValue } from "#/lib/json";
import { MissingDataLossIdentities } from "#/modules/runtime/data-loss-confirm";
import { dataLossIdentitySchema } from "#/modules/runtime/data-loss-identity";
import { RuntimeConnectionFailure } from "#/modules/runtime/runtime-connection-errors";

// SAFETY: the package exports this named CommonJS SDK surface at runtime.
const { connect: connectSdk, ployzVersion } = createRequire(import.meta.url)("@ployz/sdk") as Pick<typeof PloyzSdk, "connect" | "ployzVersion">;

/** The release this SDK speaks; it needs no Machine. */
export { ployzVersion };

export class PloyzProviderError extends Data.TaggedError(
  "PloyzProviderError",
)<{
  readonly operation: string;
  readonly cause: unknown;
}> {}

export type PloyzSdkError =
  | PloyzProviderError
  | MissingDataLossIdentities;

export interface PloyzSession {
  readonly logs: (options: LogOptions) => Effect.Effect<AsyncIterable<LogEvent>, PloyzSdkError>;
  readonly logHistory: (options: LogHistoryOptions) => Effect.Effect<LogHistoryPage, PloyzSdkError>;
  readonly inspect: () => Effect.Effect<MachineDetails, PloyzSdkError>;
  /** Resolves to the secret `ployz1:` Management Capability for `label`. */
  readonly setManagementClient: (label: string) => Effect.Effect<string, PloyzSdkError>;
  readonly clearManagementClient: (label: string) => Effect.Effect<void, PloyzSdkError>;
  readonly observeEnrollment: () => Effect.Effect<EnrollmentSnapshot, PloyzProviderError>;
  readonly register: (assignment: EnrollmentAssignment) => Effect.Effect<JsonValue, PloyzProviderError>;
  /** Resolves to why the reset didn't finish, if it didn't: the Server left the Cluster, but may keep its state. */
  readonly removeMachine: (
    machine: MachineTarget,
    confirmDataLoss: DataLossConfirmation,
  ) => Effect.Effect<LocalMachineRemoved, PloyzSdkError>;
  /** Take a Server out of the Cluster without resetting it: it keeps its state and keys. */
  readonly removeMachineMembership: (machine: MachineTarget) => Effect.Effect<void, PloyzSdkError>;
  readonly dataLossIfMachineRemoved: (
    machine: MachineTarget,
  ) => Effect.Effect<ObservedDataLoss, PloyzSdkError>;
  readonly updateMachine: (
    machine: MachineTarget,
    update: Partial<MachineUpdate>,
  ) => Effect.Effect<void, PloyzSdkError>;
  /** What removing Namespace `namespace`, its Volumes included, deletes. */
  readonly dataLossIfNamespaceDestroyed: (namespace: string) => Effect.Effect<ObservedDataLoss, PloyzSdkError>;
  /** Remove Namespace `namespace` from every Server, its Volumes included, accepting exactly `confirmDataLoss`. */
  readonly destroyNamespace: (namespace: string, confirmDataLoss: DataLossConfirmation) => Effect.Effect<DeployOutcome<ExecutionError>, PloyzSdkError>;
  readonly publishCertificateMaterial: (
    request: PublishCertificateMaterialRequest,
  ) => Effect.Effect<void, PloyzSdkError>;
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

function asSdkFailure(operation: string, cause: unknown): PloyzSdkError {
  const missingDataLoss = missingDataLossFromSdkError(cause);
  if (missingDataLoss !== null) return missingDataLoss;
  if (cause instanceof MissingDataLossIdentities) return cause;
  if (cause instanceof PloyzProviderError) return cause;
  return new PloyzProviderError({ operation, cause });
}

/** The code of the runtime's RPC error a failure carries, such as "not_found"; undefined for any other failure. */
export function rpcErrorCode(error: PloyzSdkError) {
  const rpc = Schema.decodeUnknownOption(Schema.Struct({ code: Schema.String }))("cause" in error ? error.cause : undefined);
  return Option.isSome(rpc) ? rpc.value.code : undefined;
}

function sdkPromise<A>(operation: string, run: (signal: AbortSignal) => Promise<A>) {
  return Effect.tryPromise({
    try: run,
    catch: (cause) => asSdkFailure(operation, cause),
  });
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
    setManagementClient: (label) => sdkPromise("set Management Client", () => client.setManagementClient(label)),
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
        client.removeMachine(machine, confirmDataLoss),
      ),
    removeMachineMembership: (machine) =>
      sdkPromise("remove machine membership", () => client.removeMachineMembership(machine)),
    dataLossIfMachineRemoved: (machine) =>
      sdkPromise("load machine data loss", () =>
        client.dataLossIfMachineRemoved(machine),
      ),
    dataLossIfNamespaceDestroyed: (namespace) =>
      sdkPromise("load namespace data loss", () => client.dataLossIfNamespaceDestroyed(namespace, true)),
    destroyNamespace: (namespace, confirmDataLoss) =>
      sdkPromise("remove namespace", () => client.destroyNamespace(namespace, confirmDataLoss, true)),
    updateMachine: (machine, update) =>
      sdkPromise("update machine", () =>
        client.updateMachine(machine, update).then(() => undefined),
      ),
    publishCertificateMaterial: (request) =>
      sdkPromise("publish certificate material", () => client.publishCertificateMaterial(request).then(() => undefined)),
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
