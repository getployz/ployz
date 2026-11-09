import "@tanstack/react-start/server-only";
import { Effect, Redacted } from "effect";
import { approveKeyed, type Decider, type OperationAsked } from "#/modules/approvals/approvals.server";
import { offlineServers } from "#/modules/machines/server-status";
import { refusalWords } from "#/modules/machines/server-web-traffic.server";
import { freshOperationDigest } from "#/modules/machines/server-operations.server";
import {
  type PrivateAccessChanged,
  type PrivateAccessObservation,
  type PrivateAccessWant,
  privateAccessOn,
  routerProblem,
  routersOn,
} from "#/modules/private-access/private-access";
import { firstRuntimeFrame, OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import { Database } from "#/server/database.server";
import { Conflict, Validation } from "#/server/public-error";

const ROUTER_SETTLE_MS = 45_000;
const SET_LIMIT_MS = 2 * ROUTER_SETTLE_MS;

const refuse = (message: string) => new Validation({ message, userFacing: true });

export const observePrivateAccess = Effect.fn("PrivateAccess.observe")(function* (organizationId: string) {
  const frame = yield* firstRuntimeFrame(organizationId).pipe(Effect.catch(() => Effect.succeed(null)));
  if (frame === null) return null;
  const routers = routersOn(frame);
  const offline = offlineServers(frame.machines.map(({ machine, membership }) =>
    ({ id: machine.id, membership, runningBuilds: machine.runtime.running_builds, unanswered: frame.unanswered.includes(machine.id) })));
  return {
    servers: frame.machines.map(({ machine }) => ({ id: machine.id, name: machine.name, router: routers.get(machine.id) ?? "none" })),
    partial: frame.incomplete_ids.machines.length > 0 || frame.incomplete_ids.containers.length > 0 || offline.size > 0,
  } satisfies PrivateAccessObservation;
});

export const setPrivateAccess = Effect.fn("PrivateAccess.set")(function* (organizationId: string, authKey: Redacted.Redacted<string> | null) {
  const opened = yield* (yield* OrganizationRuntime).open(organizationId);
  if (opened.status !== "connected") return yield* refuse("Your servers aren't answering. Try again once they are.");
  const changed = yield* opened.connected.setPrivateAccess(authKey).pipe(
    Effect.mapError((error) => refuse(refusalWords(error))),
    Effect.timeoutOrElse({ duration: SET_LIMIT_MS, orElse: () => Effect.fail(refuse("Your servers didn't finish in time. Check again in a minute.")) }),
  );
  const names = new Map(changed.machines.map(({ machine, server }) => [machine, server]));
  return {
    servers: changed.machines.map(({ machine, server, router }) => ({ id: machine, name: server, router })),
    failures: changed.failures.map(({ machine_id, error }) =>
      ({ id: machine_id, name: names.get(machine_id) ?? machine_id, message: error.message, problem: routerProblem(error) })),
  } satisfies PrivateAccessChanged;
}, Effect.scoped);

export const privateAccessAsked = (organizationId: string, to: PrivateAccessWant) => ({
  subject: `cluster:${organizationId}`,
  verb: "set_private_access",
  name: to,
  preview: { to },
  effects: [],
  alwaysAsks: true,
}) satisfies OperationAsked;

/** Turn Private Access on with the key the approver pasted, holding the Approval while the Servers take it. */
export const approvePrivateAccess = Effect.fn("PrivateAccess.approve")(function* (
  decider: Decider,
  input: { readonly id: string; readonly digest: string; readonly authKey: Redacted.Redacted<string> },
) {
  const keyed = yield* approveKeyed(
    decider,
    input,
    setPrivateAccess(decider.organization.id, input.authKey),
    (changed) => changed.failures.length === 0,
    freshOperationDigest,
  );
  if (!keyed.ok) return yield* new Conflict({ userFacing: true, message: keyed.refusal.message });
  return { changed: keyed.ran, decided: keyed.approval };
});

export const runPrivateAccess = Effect.fn("PrivateAccess.run")(function* (organizationId: string, to: PrivateAccessWant): Effect.fn.Return<PrivateAccessChanged, Validation | Error, Database | OrganizationRuntime> {
  if (to === "off") return yield* setPrivateAccess(organizationId, null);
  const observed = yield* observePrivateAccess(organizationId);
  if (observed === null || !privateAccessOn(observed)) {
    return yield* refuse("Private Access isn't on yet. Approve it in Ployz Cloud, where you paste the auth key.");
  }
  return { servers: observed.servers.map((server) => ({ ...server, router: server.router === "running" })), failures: [] };
});
