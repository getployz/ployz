import "@tanstack/react-start/server-only";
import type { PrivateAccessOn } from "@ployz/sdk";
import { Data, Effect } from "effect";
import { approveKeyed, type Decider, type OperationAsked } from "#/modules/approvals/approvals.server";
import { refusalWords } from "#/modules/machines/server-web-traffic.server";
import { freshOperationDigest } from "#/modules/machines/server-operations.server";
import { type PrivateAccessChanged, type PrivateAccessWant, privateAccessObservation, routerProblem } from "#/modules/private-access/private-access";
import { firstRuntimeFrame, OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import type { PloyzSession } from "#/modules/runtime/ployz.server";
import { Database } from "#/server/database.server";
import { Conflict, Validation } from "#/server/public-error";

const ROUTER_SETTLE_MS = 45_000;
const SET_LIMIT_MS = 2 * ROUTER_SETTLE_MS;
const UNANSWERED = "Your servers aren't answering. Try again once they are.";

const refuse = (message: string) => new Validation({ message, userFacing: true });

class StillConnecting extends Data.TaggedError("StillConnecting") {}

const waitedOut = <E>(error: E) =>
  Effect.fail(error instanceof StillConnecting ? refuse("Your servers didn't finish in time. Check again in a minute.") : error);

export const observePrivateAccess = Effect.fn("PrivateAccess.observe")(function* (organizationId: string) {
  const frame = yield* firstRuntimeFrame(organizationId).pipe(Effect.catch(() => Effect.succeed(null)));
  return frame === null ? null : privateAccessObservation(frame);
});

const connectedSession = Effect.fn("PrivateAccess.session")(function* (organizationId: string) {
  const opened = yield* (yield* OrganizationRuntime).open(organizationId);
  return opened.status === "connected" ? opened.connected : yield* refuse(UNANSWERED);
});

const send = (session: PloyzSession, on: PrivateAccessOn | null) => session.setPrivateAccess(on).pipe(
  Effect.mapError((error) => refuse(refusalWords(error))),
  Effect.timeoutOrElse({ duration: SET_LIMIT_MS, orElse: () => Effect.fail(new StillConnecting()) }),
  Effect.map((changed): PrivateAccessChanged => {
    const names = new Map(changed.machines.map(({ machine, server }) => [machine, server]));
    return {
      servers: changed.machines.map(({ machine, server, router }) => ({ id: machine, name: server, router })),
      failures: changed.failures.map(({ machine_id, error }) =>
        ({ id: machine_id, name: names.get(machine_id) ?? machine_id, message: error.message, problem: routerProblem(error) })),
    };
  }),
  Effect.catch(waitedOut),
);

/** Turn Private Access on with a key the browser sealed and the person approved, or off with `null`. */
export const setPrivateAccess = (organizationId: string, on: PrivateAccessOn | null) =>
  Effect.flatMap(connectedSession(organizationId), (session) => send(session, on)).pipe(Effect.scoped);

export const privateAccessAsked = (organizationId: string, to: PrivateAccessWant) => ({
  subject: `cluster:${organizationId}`,
  verb: "set_private_access",
  name: to,
  preview: { to },
  effects: [],
  alwaysAsks: true,
}) satisfies OperationAsked;

/** What a Connect that approved its Approval got from the Servers: their answer, or why Cloud has none. */
export type KeyedSent = PrivateAccessChanged | { readonly problem: string };

/** Approve the Approval with `approveKeyed`, then turn Private Access on with the key the approver sealed and approved. */
export const approvePrivateAccess = Effect.fn("PrivateAccess.approve")(function* (
  decider: Decider,
  input: { readonly id: string; readonly digest: string; readonly on: PrivateAccessOn },
) {
  const session = yield* connectedSession(decider.organization.id);
  const decided = yield* approveKeyed(decider, input, freshOperationDigest);
  if (!decided.ok) return yield* new Conflict({ userFacing: true, message: decided.refusal.message });
  const sent: KeyedSent = yield* send(session, input.on).pipe(
    Effect.catchTag("Validation", (problem) => Effect.succeed({ problem: problem.message })),
  );
  return { approval: decided.approval, sent };
}, Effect.scoped);

export const runPrivateAccess = Effect.fn("PrivateAccess.run")(function* (organizationId: string, to: PrivateAccessWant): Effect.fn.Return<PrivateAccessChanged, Validation | Error, Database | OrganizationRuntime> {
  if (to === "off") return yield* setPrivateAccess(organizationId, null);
  const observed = yield* observePrivateAccess(organizationId);
  if (observed === null) return yield* refuse(UNANSWERED);
  return { servers: observed.servers.map((server) => ({ ...server, router: server.router === "running" })), failures: [] };
});
