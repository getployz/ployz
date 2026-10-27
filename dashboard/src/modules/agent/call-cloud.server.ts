import "@tanstack/react-start/server-only";
import { carriedInto } from "#/modules/approvals/approvals.server";
import type { Act, AcceptedLoss, CloudQuery, CopiesAnswer, EnvironmentRef, MachineId, Operation, OperationsAnswer, OwnedNamespace, ServersAnswer, Subject, VolumeRun } from "@ployz/sdk";
import { Data, Effect } from "effect";
import { type StoreRefusal, StoreRefused, type StoreResult } from "#/modules/config-store/store.contract";
import { readStore } from "#/modules/config-store/config-store.server";
import { findService, owners, sealedNames, unreadable, unreadableEnvironment } from "#/modules/config-store/store-lookup.server";
import { githubBranches, githubConnection, GithubRepositories } from "#/modules/github/github-cli.server";
import { type Caller, callerEnvironments } from "#/modules/identity/actor";
import { requireClaimed } from "#/modules/identity/claim-required.server";
import { countOrganizationMachines, loadOrganizationConnections } from "#/modules/machines/connections.server";
import { UNREACHABLE_MESSAGE } from "#/modules/machines/server-operations.server";
import type { ServerPolicyChange } from "#/modules/machines/server-policy";
import { admittedAt, type Admission } from "#/modules/operations/keyed.server";
import { dashedAttempt, operationsOn, serverNames, settledDrain } from "#/modules/operations/operation-history.server";
import {
  actService,
  cleanNamespace,
  drainServer,
  forgetServer,
  type OperationCall,
  removeServer,
  runVolume,
  setServer,
  unheld,
  unpermitted,
  upgradeServer,
} from "#/modules/operations/operations.server";
import { OPERATIONS, VOLUME_OPERATIONS } from "#/modules/operations/operations";
import type { Permission } from "#/modules/organization/permissions";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import { observeVolumeCopies, rpcErrorCode, sdkFailureMessage } from "#/modules/runtime/ployz.server";
import type { VolumeRunInput } from "#/modules/volume-run/volume-run";
import { dockerVolumeName } from "#/modules/volume-run/volume-run";
import { refreshMirror } from "#/modules/volume-copies/volume-copies.refresh.server";
import { Conflict, NotFound, Validation } from "#/server/public-error";

export type CloudCall = { readonly ask: CloudQuery } | { readonly act: Act };

const refusal = (code: string, message: string, details: StoreRefusal["details"] = null) =>
  ({ ok: false, refusal: { code, message, details } }) as const;
const answer = <A>(value: A): StoreResult<A> => ({ ok: true, value });

const CONNECT_SERVERS = { fix: { fix: "connect_servers" } };
const unreachable = () => refusal("unavailable", UNREACHABLE_MESSAGE, CONNECT_SERVERS);
export const NO_SERVERS_MESSAGE = "This Organization has no servers yet. Add one from a terminal; staging still works without one.";
const noSuch = (what: string) => refusal("not_found", `No such ${what}.`);

/** A failure before Cloud wrote anything, so the agent may retry without checking its runs. */
class BeforeWrite<E> extends Data.TaggedError("CallCloud.BeforeWrite")<{ readonly error: E }> {}
const beforeWrite = <A, E, R>(read: Effect.Effect<A, E, R>) => Effect.mapError(read, (error) => new BeforeWrite({ error }));

const session = (organizationId: string) =>
  Effect.flatMap(OrganizationRuntime, (runtime) => runtime.open(organizationId)).pipe(
    Effect.map((opened) => opened.status === "connected" ? opened.connected : null),
  );

/** Cloud's own answer for an Organization with no Servers: it needs none of them to know. */
const noServers = (): ServersAnswer => {
  const now = Math.floor(Date.now() / 1000);
  return { servers: [], seen: { seen_by: null, observed_at: now, read_at: now, age_seconds: 0, from: "cloud_cache", unanswered: [] } };
};

/** Why Cloud reached no Server: the Organization has none yet, or its Servers aren't answering. */
const notConnected = Effect.fn("CallCloud.notConnected")(function* (organizationId: string) {
  return (yield* countOrganizationMachines(organizationId)) === 0
    ? refusal("conflict", NO_SERVERS_MESSAGE, CONNECT_SERVERS)
    : unreachable();
});

const runtimeAsk = (organizationId: string, query: CloudQuery, readable: ReadonlyArray<string> | null, sealed: ReadonlyArray<string> | null) =>
  Effect.gen(function* () {
    const connected = yield* session(organizationId);
    if (connected === null) return yield* notConnected(organizationId);
    return yield* connected.agentAsk(query, readable, sealed).pipe(
      Effect.map(answer),
      Effect.catch((error) => Effect.succeed(refusal(rpcErrorCode(error) ?? "unavailable", sdkFailureMessage(error)))),
    );
  }).pipe(Effect.scoped);

/**
 * Volume `volumeId` and its Environment, or null when no Environment has it. An Environment the Store can't read stops
 * no other's Volumes; only a Volume found nowhere else, which may be in it, refuses naming it.
 */
const findVolume = Effect.fn("CallCloud.findVolume")(function* (organizationId: string, volumeId: string) {
  let unread: OwnedNamespace | undefined;
  for (const owned of yield* owners(organizationId)) {
    const environment: EnvironmentRef = { project: owned.project, environment: owned.environment };
    const volumes = yield* readStore(organizationId, { query: "volumes", environment }).pipe(
      Effect.map((read) => read.volumes),
      Effect.catchIf(unreadable, () => Effect.succeed(null)),
    );
    if (volumes === null) {
      unread ??= owned;
      continue;
    }
    const volume = volumes.find(({ id }) => id === volumeId);
    if (volume !== undefined) return { environment, name: volume.name };
  }
  if (unread !== undefined) return yield* unreadableEnvironment(unread);
  return null;
});

const copies = Effect.fn("CallCloud.copies")(function* (caller: Caller, namespace: string, only: string | null) {
  const organizationId = caller.organization.id;
  const owned = (yield* owners(organizationId)).find((row) => row.namespace === namespace);
  // Which Server holds a copy is a read of its Environment, refused before any Server is looked at.
  const refused = unheld(caller, "volumes:read", owned?.environment ?? null);
  if (refused !== null) return refused;
  if (owned === undefined) return noSuch("Namespace");
  const { volumes } = yield* readStore(organizationId, { query: "volumes", environment: { project: owned.project, environment: owned.environment } })
    .pipe(Effect.catchIf(unreadable, () => Effect.fail(unreadableEnvironment(owned))));
  const named = volumes.filter(({ name }) => only === null || name === only);
  const loaded = yield* loadOrganizationConnections(organizationId);
  if (loaded.kind !== "ready" || loaded.connections.length === 0) return yield* notConnected(organizationId);
  const observed = yield* observeVolumeCopies(loaded.connections);
  const names = yield* serverNames(organizationId, [...observed.copies.map((copy) => copy.machine_id), ...observed.unanswered]);
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

/**
 * Read `query` from the runtime as `caller` may with `permission`. A read of one Namespace lands in its Environment,
 * so a token held to Environments reads only theirs. Such a token may not make a read of no Namespace that is
 * `organizationWide`; any other read of no Namespace describes only the Services in its Environments.
 */
const runtimeRead = Effect.fn("CallCloud.runtimeRead")(function* (
  caller: Caller, query: CloudQuery, permission: Permission, namespace: string | null,
  { organizationWide = false, sealed = null }: { readonly organizationWide?: boolean; readonly sealed?: ReadonlyArray<string> | null } = {},
) {
  const lacking = unpermitted(caller, permission);
  if (lacking !== null) return lacking;
  if (callerEnvironments(caller) === undefined) return yield* runtimeAsk(caller.organization.id, query, null, sealed);
  const owned = yield* owners(caller.organization.id);
  if (namespace !== null || organizationWide) {
    const refused = unheld(caller, permission, owned.find((row) => row.namespace === namespace)?.environment ?? null);
    if (refused !== null) return refused;
  }
  const readable = owned.filter((row) => unheld(caller, permission, row.environment) === null).map((row) => row.namespace);
  return yield* runtimeAsk(caller.organization.id, query, readable, sealed);
});

const SEALED_REREADS = 2;

/**
 * Read container logs scrubbed under the names the Store holds Sealed. A Deploy may seal a new variable and start its
 * Container while the runtime is being read, so the names are read again afterwards; a name the read didn't scrub under
 * reruns the read with it, up to `SEALED_REREADS` times, and a set that never settles withholds the logs. Only growth
 * matters: a name that left the set left nothing unscrubbed.
 */
const logsRead = Effect.fn("CallCloud.logsRead")(function* (caller: Caller, query: Extract<CloudQuery, { query: "logs" }>) {
  const organizationId = caller.organization.id;
  let sealed = yield* sealedNames(organizationId, query.namespace);
  for (let rerun = 0; ; rerun++) {
    const used = sealed;
    const heard = yield* runtimeRead(caller, query, "logs:read", query.namespace, { sealed: used });
    if (used === null || !heard.ok) return heard;
    const fresh = yield* sealedNames(organizationId, query.namespace);
    if (fresh !== null && fresh.every((name) => used.includes(name))) return heard;
    if (rerun === SEALED_REREADS) {
      return refusal("unavailable", "Sealed variables kept changing while the logs were read. Ask again.");
    }
    sealed = fresh === null ? null : [...new Set([...used, ...fresh])].sort((a, b) => a.localeCompare(b));
  }
});

const ask = Effect.fn("CallCloud.ask")(function* (caller: Caller, query: CloudQuery) {
  const organizationId = caller.organization.id;
  switch (query.query) {
    case "github_connection":
      yield* requireClaimed(organizationId, "github_connect");
      return yield* githubConnection(caller).pipe(Effect.map(({ ready, repositories, connect_url }) => answer({
        connected: ready,
        repositories: repositories.map(({ repository }) => repository),
        connect_at: connect_url,
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
    case "servers": {
      // An Organization with no Servers yet lists none, as any empty list does; the agent's tool says how to add one.
      const lacking = unpermitted(caller, "servers:read");
      if (lacking !== null) return lacking;
      if ((yield* countOrganizationMachines(organizationId)) === 0) return answer<ServersAnswer>(noServers());
      return yield* runtimeRead(caller, query, "servers:read", null);
    }
    case "containers":
      return yield* runtimeRead(caller, query, "servers:read", query.namespace);
    case "metrics":
      return yield* runtimeRead(caller, query, "metrics:read", query.subject.kind === "service" ? query.subject.namespace : null);
    case "copies":
      return yield* copies(caller, query.namespace, query.volume);
    case "operations": {
      const operations = yield* operationsOn(organizationId, query.subject, query.attempt, query.limit);
      const settled = query.subject.kind === "server" && query.attempt === null ? yield* settledDrain(organizationId, query.subject.machine, null) : null;
      return answer<OperationsAnswer>(settled === null ? { operations } : { operations, settled_drain: settled });
    }
    case "logs":
      if (caller.support !== null && !caller.support.logs) {
        return refusal("forbidden", "This Organization doesn't show its container logs to Ployz support.");
      }
      return yield* logsRead(caller, query);
    case "server_logs":
      // The support logs switch promises only Container output, so it never opens a Server's own logs.
      if (caller.support !== null) {
        return refusal("forbidden", "Ployz support can't read an Organization's Server logs.");
      }
      // A Server's own processes log for every Environment it runs, so a token held to some may not read them.
      return yield* runtimeRead(caller, query, "logs:read", null, { organizationWide: true });
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
      return { kind: "delete_mirror", args: { slot: null, confirmed_name: name, role: "any" } };
    case "restore":
      return { kind: "restore", args: { from: run.from.name, machine: run.from.machine } };
  }
};

const lossOf = (action: Act["action"]): AcceptedLoss | null | undefined =>
  "loss" in action ? action.loss : action.action === "volume" && "loss" in action.run ? action.run.loss : undefined;

const policyChangeOf = (set: Extract<Act["action"], { action: "set_server" }>): ServerPolicyChange => ({
  ...(set.accepts_builds !== null && { acceptsBuilds: set.accepts_builds }),
  ...(set.accepts_services !== null && { acceptsServices: set.accepts_services }),
  ...(set.accepts_ingress === false && { acceptsIngress: false }),
  ...(set.build_concurrency.action === "automatic" && { buildConcurrency: "automatic" }),
  ...(set.build_concurrency.action === "set" && { buildConcurrency: set.build_concurrency.value }),
});

const act = Effect.fn("CallCloud.act")(function* (caller: Caller, { attempt, digest, action }: Act, approvalId: string | null) {
  const organizationId = caller.organization.id;
  const call: OperationCall = { key: attempt, approvalId, loss: lossOf(action) };
  const admitted = (admission: Admission | null, subject: Subject, what: string, serverName: string | null = null) => Effect.gen(function* () {
    if (admission === null) return noSuch(what);
    if (!admission.ok) return { ok: false, refusal: admission.refusal } as const;
    const id = dashedAttempt(admission.id);
    const [run] = yield* operationsOn(organizationId, subject, id, 1, serverName);
    const requested = run === undefined ? yield* admittedAt(organizationId, attempt) : null;
    return answer<Operation>(run ?? {
      attempt: id, run: what, state: "accepted", requested_at: Math.floor((requested ?? new Date()).getTime() / 1000), ended_at: null, reason: null, servers: [], kept: [], unanswered: [],
    });
  });
  switch (action.action) {
    case "service": {
      const lacking = unpermitted(caller, OPERATIONS[`service.${action.verb}`].requires);
      if (lacking !== null) return lacking;
      const found = yield* beforeWrite(findService(organizationId, action.environment, action.service));
      if (found === null) return noSuch("Service");
      const admission = yield* actService(caller, { environmentId: action.environment, service: action.service, ...found, verb: action.verb, only: action.only, digest }, call);
      return yield* admitted(admission, { kind: "service", environment: action.environment, service: action.service }, "Service");
    }
    case "drain":
      return yield* admitted(yield* drainServer(caller, action.server.machine, call, action.server.name), { kind: "server", machine: action.server.machine }, "Server", action.server.name);
    case "upgrade":
      return yield* admitted(
        yield* upgradeServer(caller, { machineId: action.server.machine, channel: undefined }, call, action.server.name),
        { kind: "server", machine: action.server.machine },
        "Server",
        action.server.name,
      );
    case "set_server": {
      if (action.accepts_ingress === true) return refusal("invalid_argument", "Turn web traffic on from the Server page or the terminal.");
      return yield* admitted(
        yield* setServer(caller, action.server.machine, policyChangeOf(action), call),
        { kind: "server", machine: action.server.machine },
        `set ${action.server.name}`,
        action.server.name,
      );
    }
    case "clean":
      return yield* admitted(yield* cleanNamespace(caller, action.leftover, call), { kind: "leftover", namespace: action.leftover }, "leftover");
    case "remove_server": {
      // Before the Servers are asked what the removal loses, so a token that can't remove Servers hears why, not that they're down.
      const refused = unheld(caller, OPERATIONS["server.remove"].requires);
      if (refused !== null) return refused;
      const connected = yield* beforeWrite(session(organizationId));
      if (connected === null) return yield* beforeWrite(notConnected(organizationId));
      const { data_loss } = yield* beforeWrite(connected.dataLossIfMachineRemoved(action.server.machine));
      return yield* admitted(
        yield* removeServer(caller, action.server.machine, data_loss, call, action.server.name),
        { kind: "server", machine: action.server.machine },
        "Server",
        action.server.name,
      );
    }
    case "forget_server":
      return yield* admitted(
        yield* forgetServer(caller, action.server.machine, call, action.server.name),
        { kind: "server", machine: action.server.machine },
        "Server",
        action.server.name,
      );
    case "volume": {
      // Its Environment is known only once found; the permission is known now, so a token without it never waits on Servers.
      const quickSync = action.run.run === "sync" && !action.run.full;
      const lacking = unpermitted(caller, quickSync ? OPERATIONS["volume.sync"].requires : VOLUME_OPERATIONS[volumeRunInput(action.run, "").kind].requires);
      if (lacking !== null) return lacking;
      // A quick sync asks no one, and still refuses an approval carried into it that isn't for exactly this sync.
      const carried = quickSync ? yield* carriedInto(caller, approvalId, { command: "sync", subject: `volume:${action.volume}` }) : null;
      if (carried !== null) return { ok: false, refusal: carried } as const;
      const found = yield* beforeWrite(findVolume(organizationId, action.volume));
      if (found === null) return noSuch("Volume");
      if (quickSync) {
        const unsynced = unheld(caller, OPERATIONS["volume.sync"].requires, found.environment.environment);
        if (unsynced !== null) return unsynced;
        const synced = yield* refreshMirror({ userId: caller.userId, organizationId }, found.environment, action.volume);
        if (!synced.ok) return synced;
        const now = Math.floor(Date.now() / 1000);
        return answer<Operation>({
          attempt: dashedAttempt(attempt), run: `sync ${found.name}`, state: "done", requested_at: now, ended_at: now,
          reason: synced.lines.join("\n"), servers: [], kept: [], unanswered: [],
        });
      }
      const input = { volumeId: action.volume, environment: found.environment, ...volumeRunInput(action.run, found.name) };
      return yield* admitted(yield* runVolume(caller, input, call), { kind: "volume", volume: action.volume }, "Volume");
    }
  }
}, Effect.scoped);

type CloudServices = Effect.Services<ReturnType<typeof act>> | Effect.Services<ReturnType<typeof ask>>;
type CloudError = Effect.Error<ReturnType<typeof act>> | Effect.Error<ReturnType<typeof ask>>;

type Failure = CloudError extends infer E ? E extends BeforeWrite<infer Read> ? Read : E : never;

const refusalOf = (error: Failure, acting: boolean) => {
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
    Effect.catch((error) => Effect.succeed(error instanceof BeforeWrite ? refusalOf(error.error, false) : refusalOf(error, "act" in call))),
    Effect.withSpan("CallCloud", { attributes: { call: "act" in call ? call.act.action.action : call.ask.query } }),
  );

