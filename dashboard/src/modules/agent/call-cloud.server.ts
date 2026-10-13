import "@tanstack/react-start/server-only";
import type { Act, AcceptedLoss, CloudQuery, CopiesAnswer, EnvironmentRef, MachineId, Operation, OperationsAnswer, Subject, VolumeRun } from "@ployz/sdk";
import { Effect } from "effect";
import { type StoreRefusal, StoreRefused, type StoreResult } from "#/modules/config-store/store.contract";
import { readStore } from "#/modules/config-store/config-store.server";
import { findService, owners } from "#/modules/config-store/store-lookup.server";
import { githubBranches, githubConnection, GithubRepositories } from "#/modules/github/github-cli.server";
import type { Caller } from "#/modules/identity/actor";
import { loadOrganizationConnections } from "#/modules/machines/connections.server";
import { UNREACHABLE_MESSAGE } from "#/modules/machines/server-operations.server";
import type { Admission } from "#/modules/operations/keyed.server";
import { dashedAttempt, operationsOn, serverNames } from "#/modules/operations/operation-history.server";
import {
  actService,
  cleanNamespace,
  drainServer,
  type OperationCall,
  removeServer,
  runVolume,
  upgradeServer,
} from "#/modules/operations/operations.server";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import { observeVolumeCopies, rpcErrorCode, sdkFailureMessage } from "#/modules/runtime/ployz.server";
import type { VolumeRunInput } from "#/modules/volume-run/volume-run";
import { dockerVolumeName } from "#/modules/volume-run/volume-run";
import { Conflict, NotFound, Validation } from "#/server/public-error";

export type CloudCall = { readonly ask: CloudQuery } | { readonly act: Act };

const refusal = (code: string, message: string, details: StoreRefusal["details"] = null) =>
  ({ ok: false, refusal: { code, message, details } }) as const;
const answer = <A>(value: A): StoreResult<A> => ({ ok: true, value });

const CONNECT_SERVERS = { fix: { fix: "connect_servers" } };
const unreachable = () => refusal("unavailable", UNREACHABLE_MESSAGE, CONNECT_SERVERS);
const noSuch = (what: string) => refusal("not_found", `No such ${what}.`);

const session = (organizationId: string) =>
  Effect.flatMap(OrganizationRuntime, (runtime) => runtime.open(organizationId)).pipe(
    Effect.map((opened) => opened.status === "connected" ? opened.connected : null),
  );

const runtimeAsk = (organizationId: string, query: CloudQuery) =>
  Effect.gen(function* () {
    const connected = yield* session(organizationId);
    if (connected === null) return unreachable();
    return yield* connected.agentAsk(query).pipe(
      Effect.map(answer),
      Effect.catch((error) => Effect.succeed(refusal(rpcErrorCode(error) ?? "unavailable", sdkFailureMessage(error)))),
    );
  }).pipe(Effect.scoped);

const findVolume = Effect.fn("CallCloud.findVolume")(function* (organizationId: string, volumeId: string) {
  for (const owned of yield* owners(organizationId)) {
    const environment: EnvironmentRef = { project: owned.project, environment: owned.environment };
    const { volumes } = yield* readStore(organizationId, { query: "volumes", environment });
    const volume = volumes.find(({ id }) => id === volumeId);
    if (volume !== undefined) return { environment, name: volume.name };
  }
  return null;
});

const copies = Effect.fn("CallCloud.copies")(function* (organizationId: string, namespace: string, only: string | null) {
  const owned = (yield* owners(organizationId)).find((row) => row.namespace === namespace);
  if (owned === undefined) return noSuch("Namespace");
  const { volumes } = yield* readStore(organizationId, { query: "volumes", environment: { project: owned.project, environment: owned.environment } });
  const named = volumes.filter(({ name }) => only === null || name === only);
  const loaded = yield* loadOrganizationConnections(organizationId);
  if (loaded.kind !== "ready" || loaded.connections.length === 0) return unreachable();
  const observed = yield* observeVolumeCopies(loaded.connections);
  const names = yield* serverNames(organizationId);
  const server = (machine: MachineId) => ({ machine, name: names.get(machine) ?? machine });
  const now = Math.floor(Date.now() / 1000);
  return answer<CopiesAnswer>({
    volumes: named.map(({ id, name }) => {
      const held = observed.copies.filter((copy) => copy.name === dockerVolumeName(namespace, id));
      const writer = held.find(({ role }) => role !== "slot");
      const mirror = held.find(({ role }) => role === "slot");
      return {
        volume: name,
        writer: writer === undefined ? null : server(writer.machine_id),
        mirror: mirror === undefined ? null : { server: server(mirror.machine_id), synced_at: null },
      };
    }),
    seen: {
      seen_by: null,
      observed_at: now,
      read_at: now,
      age_seconds: 0,
      from: "server",
      unanswered: observed.unanswered.map((machine) => names.get(machine) ?? machine),
    },
  });
});

const ask = Effect.fn("CallCloud.ask")(function* (caller: Caller, query: CloudQuery) {
  const organizationId = caller.organization.id;
  switch (query.query) {
    case "github_connection":
      return yield* githubConnection(caller).pipe(Effect.map(({ ready, repositories, install_url }) => answer({
        connected: ready,
        repositories: repositories.map(({ repository }) => repository),
        connect_at: install_url,
      })));
    case "github_branches":
      return yield* githubBranches(caller, query.repository).pipe(Effect.map(answer));
    case "github_tree": {
      const { query: _, ...tree } = query;
      return yield* Effect.flatMap(GithubRepositories, (github) => github.tree(caller, tree)).pipe(Effect.map(answer));
    }
    case "github_file": {
      const { query: _, ...file } = query;
      return yield* Effect.flatMap(GithubRepositories, (github) => github.file(caller, file)).pipe(Effect.map(answer));
    }
    case "servers":
    case "containers":
    case "metrics":
      return yield* runtimeAsk(organizationId, query);
    case "copies":
      return yield* copies(organizationId, query.namespace, query.volume);
    case "operations":
      return yield* operationsOn(organizationId, query.subject, query.attempt, query.limit).pipe(
        Effect.map((operations) => answer<OperationsAnswer>({ operations })),
      );
    case "logs":
      if (caller.support !== null && !caller.support.logs) {
        return refusal("forbidden", "This Organization doesn't show its container logs to Ployz support.");
      }
      return yield* runtimeAsk(organizationId, query);
  }
});

const volumeRunInput = (run: VolumeRun, name: string): VolumeRunInput => {
  switch (run.run) {
    case "mirror":
      return { kind: "mirror", args: { to: run.to.name, machine: run.to.machine } };
    case "sync":
      return { kind: "sync", args: { full: run.full } };
    case "move":
      return { kind: "move", args: { to: run.to.name, machine: run.to.machine } };
    case "release":
      return { kind: "release", args: {} };
    case "remove_mirror":
      return { kind: "delete_mirror", args: { slot: null, confirmed_name: name } };
  }
};

const lossOf = (action: Act["action"]): AcceptedLoss | null | undefined =>
  "loss" in action ? action.loss : action.action === "volume" && "loss" in action.run ? action.run.loss : undefined;

const act = Effect.fn("CallCloud.act")(function* (caller: Caller, { attempt, digest, action }: Act, approvalId: string | null) {
  const organizationId = caller.organization.id;
  const call: OperationCall = { key: attempt, approvalId, loss: lossOf(action) };
  const admitted = (admission: Admission | null, subject: Subject, what: string) => Effect.gen(function* () {
    if (admission === null) return noSuch(what);
    if (!admission.ok) return { ok: false, refusal: admission.refusal } as const;
    const id = dashedAttempt(admission.id);
    const [run] = yield* operationsOn(organizationId, subject, id, 1);
    return answer<Operation>(run ?? {
      attempt: id, run: what, state: "accepted", requested_at: Math.floor(Date.now() / 1000), ended_at: null, reason: null, servers: [], kept: [], unanswered: [],
    });
  });
  switch (action.action) {
    case "service": {
      const found = yield* findService(organizationId, action.environment, action.service);
      if (found === null) return noSuch("Service");
      const admission = yield* actService(caller, { environmentId: action.environment, service: action.service, ...found, verb: action.verb, only: action.only, digest }, call);
      return yield* admitted(admission, { kind: "service", environment: action.environment, service: action.service }, "Service");
    }
    case "drain":
      return yield* admitted(yield* drainServer(caller, action.server.machine, call), { kind: "server", machine: action.server.machine }, "Server");
    case "upgrade":
      return yield* admitted(
        yield* upgradeServer(caller, { machineId: action.server.machine, channel: undefined }, call),
        { kind: "server", machine: action.server.machine },
        "Server",
      );
    case "clean":
      return yield* admitted(yield* cleanNamespace(caller, action.leftover, call), { kind: "leftover", namespace: action.leftover }, "leftover");
    case "remove_server": {
      const connected = yield* session(organizationId);
      if (connected === null) return unreachable();
      const { data_loss } = yield* connected.dataLossIfMachineRemoved(action.server.machine);
      return yield* admitted(
        yield* removeServer(caller, action.server.machine, data_loss, call),
        { kind: "server", machine: action.server.machine },
        "Server",
      );
    }
    case "volume": {
      const found = yield* findVolume(organizationId, action.volume);
      if (found === null) return noSuch("Volume");
      const input = { volumeId: action.volume, environment: found.environment, ...volumeRunInput(action.run, found.name) };
      return yield* admitted(yield* runVolume(caller, input, call), { kind: "volume", volume: action.volume }, "Volume");
    }
  }
}, Effect.scoped);

type CloudServices = Effect.Services<ReturnType<typeof act>> | Effect.Services<ReturnType<typeof ask>>;
type CloudError = Effect.Error<ReturnType<typeof act>> | Effect.Error<ReturnType<typeof ask>>;

const refusalOf = (error: CloudError, acting: boolean) => {
  if (error instanceof StoreRefused) return refusal(error.code, error.message, error.details);
  if (error instanceof NotFound) return refusal("not_found", error.message);
  if (error instanceof Validation) return refusal("invalid_argument", error.message);
  if (error instanceof Conflict) return error.message === UNREACHABLE_MESSAGE ? unreachable() : refusal("conflict", error.message);
  return acting
    ? refusal("unavailable", "Cloud couldn't finish this. It may have reached your servers; check its runs.", { writes: "unknown" })
    : refusal("unavailable", "Cloud couldn't answer this. Try again.");
};

export const callCloud = (caller: Caller, call: CloudCall, approvalId: string | null = null) =>
  Effect.gen(function* (): Effect.fn.Return<StoreResult<unknown>, CloudError, CloudServices> {
    return "act" in call ? yield* act(caller, call.act, approvalId) : yield* ask(caller, call.ask);
  }).pipe(
    Effect.catch((error) => Effect.succeed(refusalOf(error, "act" in call))),
    Effect.withSpan("CallCloud", { attributes: { call: "act" in call ? call.act.action.action : call.ask.query } }),
  );

