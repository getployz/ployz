import { Effect, Schema } from "effect";
import { Uuid } from "#/lib/schema";
import type { Caller } from "#/modules/identity/actor";
import type { MachineRemoveAttemptView } from "#/modules/machines/machine-removal";
import { loadAuthorizedMachineRemoveAttempt } from "#/modules/machines/machine-removal.repository";
import { checkForgetServers, forgetServers, forgetServersAsked } from "#/modules/machines/forget-servers.server";
import { readCliNamespaceCleanup } from "#/modules/machines/namespace-cleanup.server";
import { readCliServerDrain } from "#/modules/machines/server-drain.server";
import { provideServerAccess } from "#/modules/machines/server-access.server";
import { planClean } from "#/modules/machines/server-operations.server";
import { CleanNamespace, DrainServer, ForgetServer, ForgetServers, RemoveServer } from "#/modules/operations/cli-wire";
import { cleanNamespace, drainServer, forgetServer, removeServer } from "#/modules/operations/operations.server";
import {
  accepted,
  admitted,
  approvalCarried,
  decodeBody,
  gated,
  keyedCall,
  missingRefusal,
  noSuchServer,
  serverIdOf,
  type CliRoutes,
} from "#/modules/commands/cli-call.server";
import { cliRefusalResponse, NotFound } from "#/server/public-error";

const forgetter = (caller: Caller) => ({ userId: caller.userId, credential: caller.credential, organizationId: caller.organization.id });

/** A Server removal as `ployz server rm` follows it: under way, settled, or refused as it ended. */
function serverRemoval(attempt: MachineRemoveAttemptView) {
  switch (attempt.state) {
    case "pending":
    case "running":
      return { state: attempt.state };
    case "succeeded":
      return {
        state: attempt.state,
        release: attempt.result.release,
      };
    case "missing_identities":
      return cliRefusalResponse({
        code: "invalid_argument",
        message: "The Server holds Volumes this removal didn't confirm. No changes made.",
        details: { missing: attempt.missingIdentities },
      });
    case "failed":
    case "cancelled":
      return cliRefusalResponse({ code: "unavailable", message: attempt.failureMessage, details: null });
    default: {
      const _exhaustive: never = attempt;
      throw new Error(`Unhandled machine remove attempt: ${String(_exhaustive)}`);
    }
  }
}

/** Removing, draining, forgetting and cleaning up after Servers Cloud manages, and reading how those runs went. */
export const serversCliRoutes = {
  "DELETE servers/:id": ({ caller, request, id }) =>
    Effect.gen(function* () {
      const machineId = serverIdOf(id);
      if (machineId === null) return noSuchServer();
      const input = yield* decodeBody(RemoveServer, request, "Removing a Server takes the Data Loss it confirms.");
      return yield* keyedCall(request, (call) =>
        Effect.map(removeServer(caller, machineId, input.confirm_data_loss.confirmed, call, input.server_name), (admission) =>
          admitted(admission, (attemptId) => ({ id: attemptId }))),
      );
    }),
  "POST servers/:id/forget": ({ caller, request, id }) =>
    Effect.gen(function* () {
      const machineId = serverIdOf(id);
      if (machineId === null) return noSuchServer();
      const input = yield* decodeBody(ForgetServer, request, "Forgetting a Server takes no options.");
      return yield* keyedCall(request, (call) =>
        Effect.map(forgetServer(caller, machineId, call, input.server_name), (admission) =>
          admitted(admission, (attemptId) => ({ id: attemptId }))),
      );
    }),
  "GET server-removals/:id": ({ caller, id }) =>
    Effect.gen(function* () {
      const attempt = Schema.is(Uuid)(id)
        ? yield* loadAuthorizedMachineRemoveAttempt({ attemptId: id, organizationId: caller.organization.id })
        : null;
      if (attempt === null) return yield* new NotFound({ message: "No such Server removal." });
      return serverRemoval(attempt);
    }),
  "POST servers/:id/drain": ({ caller, request, id }) =>
    Effect.gen(function* () {
      const machineId = serverIdOf(id);
      if (machineId === null) return noSuchServer();
      const shown = request.body === null ? undefined : (yield* decodeBody(DrainServer, request, "A drain takes no options.")).server_name;
      return yield* keyedCall(request, (call) =>
        Effect.map(drainServer(caller, machineId, call, shown), (admission) => admitted(admission, accepted)),
      );
    }),
  "GET server-drains/:id": ({ caller, id }) =>
    Effect.gen(function* () {
      const drain = Schema.is(Uuid)(id) ? yield* readCliServerDrain(caller.organization.id, id) : null;
      return drain ?? missingRefusal("No such drain.");
    }),
  "GET namespaces/:id/clean": ({ caller, id }) =>
    Effect.gen(function* () {
      const namespace = decodeURIComponent(id ?? "");
      const plan = yield* planClean(caller.organization.id, namespace);
      return { namespace, volumes: plan.doomed.map(({ identity, label }) => ({ ...identity.id, label })) };
    }),
  "POST namespaces/:id/clean": ({ caller, request, id }) =>
    Effect.gen(function* () {
      if (request.body !== null) yield* decodeBody(CleanNamespace, request, "A clean takes no body.");
      return yield* keyedCall(request, (call) =>
        Effect.map(cleanNamespace(caller, decodeURIComponent(id ?? ""), call), (admission) => admitted(admission, accepted)),
      );
    }),
  "GET namespace-cleanups/:id": ({ caller, id }) =>
    Effect.gen(function* () {
      const cleanup = Schema.is(Uuid)(id) ? yield* readCliNamespaceCleanup(caller.organization.id, id) : null;
      return cleanup ?? missingRefusal("No such namespace clean.");
    }),
  "GET forget-servers": ({ caller }) =>
    Effect.gen(function* () {
      const checked = yield* checkForgetServers(forgetter(caller));
      return checked.ok ? { organization: caller.organization.slug, ...checked.value } : cliRefusalResponse(checked.refusal);
    }),
  "POST forget-servers": ({ caller, request }) =>
    Effect.gen(function* () {
      const input = yield* decodeBody(ForgetServers, request, "Forgetting the Servers takes the Organization's slug.");
      if (input.organization !== caller.organization.slug) {
        return cliRefusalResponse({
          code: "invalid_argument",
          message: `This credential acts in Organization ${caller.organization.slug}, not ${input.organization}. No changes made.`,
          details: { next: `ployz org use ${input.organization}` },
        });
      }
      const carried = approvalCarried(request);
      if (carried !== null) return carried;
      const forgotten = yield* forgetServers(forgetter(caller), (checked) => gated(caller, request)(forgetServersAsked(caller.organization, checked)));
      return forgotten.ok ? { organization: caller.organization.slug, ...forgotten.value } : cliRefusalResponse(forgotten.refusal);
    }),
  "POST server-access": ({ caller }) =>
    Effect.gen(function* () {
      const access = yield* provideServerAccess(caller);
      return access.ok ? access.value : cliRefusalResponse(access.refusal);
    }),
} satisfies CliRoutes;
