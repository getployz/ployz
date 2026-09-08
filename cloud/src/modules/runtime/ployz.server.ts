import "@tanstack/react-start/server-only";
import { createRequire } from "node:module";
import type {
  Client,
  ClusterTeardown,
  ConnectOptions,
  DataLossConfirmation,
  DeployOutcome,
  DeployIntent,
  ExecutionError,
  HeldRegister,
  MachineId,
  MachineTarget,
  ObservedDataLoss,
  PreparedDeploy,
  ProjectName,
  RegisterRequest,
  RemoveVolumesRequest,
  RuntimeWatchView,
  WatchOptions,
  VolumeRemoval,
} from "@ployz/sdk";
import type * as PloyzSdk from "@ployz/sdk";
import { Context, Data, Effect, Layer, Option, Schema, type Scope } from "effect";
import type { JsonValue } from "#/db/tables";
import { projectJsonValue } from "#/lib/json";
import { MissingDataLossIdentities } from "#/modules/runtime/data-loss-confirm";
import { dataLossIdentitySchema } from "#/modules/runtime/data-loss-identity";
import { RuntimeConnectionFailure } from "#/modules/runtime/runtime-connection-errors";

// SAFETY: the package exports this named CommonJS SDK surface at runtime.
const {
  connect: connectSdk,
  listHeld,
  register: registerSdk,
  revokePairing,
} = createRequire(import.meta.url)("@ployz/sdk") as Pick<
  typeof PloyzSdk,
  "connect" | "listHeld" | "register" | "revokePairing"
>;

export class PloyzProviderError extends Data.TaggedError(
  "PloyzProviderError",
)<{
  readonly operation: string;
  readonly cause: unknown;
}> {}

export type PloyzSdkError =
  | PloyzProviderError
  | MissingDataLossIdentities;

export type PloyzPreparedDeploy = Omit<PreparedDeploy, "confirm"> & {
  readonly confirm: () => Effect.Effect<unknown, PloyzSdkError>;
};

export interface PloyzSession {
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

type PloyzBindings = {
  readonly connect: (options: ConnectOptions) => Promise<Client>;
  readonly listHeld?: (
    relayUrl: string,
    bearer: string,
    pairing: string,
  ) => Promise<HeldRegister[]>;
  readonly register?: (
    relayUrl: string,
    bearer: string,
    pairing: string,
    machineId: MachineId,
    identity: RegisterRequest,
  ) => Promise<object>;
  readonly revokePairing?: (
    relayUrl: string,
    bearer: string,
    pairing: string,
  ) => Promise<void>;
};

export interface PloyzService {
  readonly connect: (
    options: ConnectOptions,
  ) => Effect.Effect<PloyzSession, PloyzProviderError, Scope.Scope>;
  readonly listHeldRegisters: (
    relayUrl: string,
    bearer: string,
    pairing: string,
  ) => Effect.Effect<HeldRegister[], PloyzProviderError>;
  readonly revokeRelayPairing: (
    relayUrl: string,
    bearer: string,
    pairing: string,
  ) => Effect.Effect<void, PloyzProviderError>;
  readonly registerHeldMachine: (
    relayUrl: string,
    bearer: string,
    pairing: string,
    machineId: string,
    identity: RegisterRequest,
  ) => Effect.Effect<JsonValue, PloyzProviderError>;
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

function sdkPromise<A>(operation: string, run: (signal: AbortSignal) => Promise<A>) {
  return Effect.tryPromise({
    try: run,
    catch: (cause) => asSdkFailure(operation, cause),
  });
}

function wrapPrepared(prepared: PreparedDeploy): PloyzPreparedDeploy {
  return {
    ...prepared,
    confirm: () =>
      sdkPromise("confirm", async (signal) => {
        const running = prepared.confirm({ signal });
        const abort = () => running.abort();
        signal.addEventListener("abort", abort, { once: true });
        try {
          let outcome: unknown;
          for await (const event of running) {
            if (event.type === "outcome") outcome = event.outcome;
          }
          return outcome ?? (await running.finished);
        } finally {
          signal.removeEventListener("abort", abort);
        }
      }),
  };
}

function wrapClient(client: Client): PloyzSession {
  const watch = (options?: WatchOptions) =>
    Effect.try({
      try: () => client.runtime.watch(options),
      catch: (cause) => new RuntimeConnectionFailure({ cause }),
    });
  return {
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
  const held = bindings.listHeld ?? listHeld;
  const register = bindings.register ?? registerSdk;
  const revoke = bindings.revokePairing ?? revokePairing;
  return Layer.succeed(Ployz, {
    connect: (options) =>
      Effect.acquireRelease(
        Effect.tryPromise({
          try: () => bindings.connect(options),
          catch: (cause) =>
            new PloyzProviderError({ operation: "connect", cause }),
        }),
        closeSession,
      ).pipe(Effect.map(wrapClient)),
    listHeldRegisters: (relayUrl, bearer, pairing) =>
      Effect.tryPromise({
        try: () => held(relayUrl, bearer, pairing),
        catch: (cause) =>
          new PloyzProviderError({ operation: "list held registers", cause }),
      }),
    revokeRelayPairing: (relayUrl, bearer, pairing) =>
      Effect.tryPromise({
        try: () => revoke(relayUrl, bearer, pairing),
        catch: (cause) =>
          new PloyzProviderError({ operation: "revoke relay pairing", cause }),
      }),
    registerHeldMachine: (relayUrl, bearer, pairing, machineId, identity) =>
      Effect.gen(function* () {
        const registered = yield* Effect.tryPromise({
          try: () =>
            register(
              relayUrl,
              bearer,
              pairing,
              // SAFETY: Cloud machine ids are the same strings rust brands as MachineId.
              machineId as MachineId,
              identity,
            ),
          catch: (cause) =>
            new PloyzProviderError({
              operation: "register held machine",
              cause,
            }),
        });
        const registration = projectJsonValue(registered);
        if (registration === undefined) {
          return yield* new PloyzProviderError({
            operation: "decode held machine registration",
            cause: new Error("held Register returned a non-JSON payload"),
          });
        }
        return registration;
      }),
  });
}

export const PloyzLive = makePloyzLayer({
  connect: connectSdk,
  listHeld,
  register: registerSdk,
  revokePairing,
});
