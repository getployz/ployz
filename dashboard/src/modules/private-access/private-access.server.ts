import "@tanstack/react-start/server-only";
import { Cause, Data, Effect, Exit, Option, Redacted } from "effect";
import type { ClaimOutcome } from "#/modules/approvals/approvals";
import { approveKeyed, type Decider, type OperationAsked } from "#/modules/approvals/approvals.server";
import { refusalWords } from "#/modules/machines/server-web-traffic.server";
import { freshOperationDigest } from "#/modules/machines/server-operations.server";
import {
  type PrivateAccessChanged,
  type PrivateAccessWant,
  privateAccessObservation,
  privateAccessOn,
  routerIds,
  routerProblem,
} from "#/modules/private-access/private-access";
import { firstRuntimeFrame, OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
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

const sendPrivateAccess = Effect.fn("PrivateAccess.send")(function* (organizationId: string, authKey: Redacted.Redacted<string> | null) {
  const opened = yield* (yield* OrganizationRuntime).open(organizationId);
  if (opened.status !== "connected") return yield* refuse(UNANSWERED);
  const changed = yield* opened.connected.setPrivateAccess(authKey).pipe(
    Effect.mapError((error) => refuse(refusalWords(error))),
    Effect.timeoutOrElse({ duration: SET_LIMIT_MS, orElse: () => Effect.fail(new StillConnecting()) }),
  );
  const names = new Map(changed.machines.map(({ machine, server }) => [machine, server]));
  return {
    servers: changed.machines.map(({ machine, server, router }) => ({ id: machine, name: server, router })),
    failures: changed.failures.map(({ machine_id, error }) =>
      ({ id: machine_id, name: names.get(machine_id) ?? machine_id, message: error.message, problem: routerProblem(error) })),
  } satisfies PrivateAccessChanged;
}, Effect.scoped);

export const setPrivateAccess = (organizationId: string, authKey: Redacted.Redacted<string> | null) =>
  sendPrivateAccess(organizationId, authKey).pipe(Effect.catch(waitedOut));

const routersBefore = Effect.fn("PrivateAccess.routersBefore")(function* (organizationId: string) {
  const frame = yield* firstRuntimeFrame(organizationId).pipe(Effect.catch(() => Effect.succeed(null)));
  return frame === null ? yield* refuse(UNANSWERED) : routerIds(frame);
});

/** The SDK fails only before any Server gets the key, so every failure but Cloud's own timeout or a defect failed. */
function answered(sent: Exit.Exit<PrivateAccessChanged, unknown>): ClaimOutcome {
  if (Exit.isSuccess(sent)) {
    const { failures } = sent.value;
    if (failures.some((failure) => failure.problem === "unreachable")) return "unknown";
    return failures.length === 0 ? "took" : "failed";
  }
  const error = Cause.findErrorOption(sent.cause);
  return Option.isSome(error) && !(error.value instanceof StillConnecting) ? "failed" : "unknown";
}

export const privateAccessAsked = (organizationId: string, to: PrivateAccessWant) => ({
  subject: `cluster:${organizationId}`,
  verb: "set_private_access",
  name: to,
  preview: { to },
  effects: [],
  alwaysAsks: true,
}) satisfies OperationAsked;

/** Turn Private Access on with the key the approver pasted, holding the Approval until Cloud knows what the Servers did. */
export const approvePrivateAccess = Effect.fn("PrivateAccess.approve")(function* (
  decider: Decider,
  input: { readonly id: string; readonly digest: string; readonly authKey: Redacted.Redacted<string> },
) {
  const keyed = yield* approveKeyed(decider, input, {
    before: routersBefore(decider.organization.id),
    run: sendPrivateAccess(decider.organization.id, input.authKey),
    outcome: answered,
  }, freshOperationDigest).pipe(Effect.catch(waitedOut));
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
