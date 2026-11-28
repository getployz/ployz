import { Effect, Schema } from "effect";
import { rustMachineIdSchema } from "#/lib/schema";
import type { Caller } from "#/modules/identity/actor";
import type { resolveCaller } from "#/modules/identity/caller.server";
import { lacks } from "#/modules/organization/access.server";
import { APPROVAL_HEADER, type Gate, gateOperation, olderApproval } from "#/modules/operations/gate.server";
import { type Admission, REQUEST_KEY_HEADER, requestKeyOf } from "#/modules/operations/keyed.server";
import type { OperationCall } from "#/modules/operations/operations.server";
import { decodeJsonBody } from "#/server/api-route";
import { cliRefusalResponse, NotFound, Validation } from "#/server/public-error";
import type { AppServices } from "#/server/runtime.server";

/** One `/api/cli/*` call: who makes it, the request, and the second path segment (`:id` in its route key). */
export interface CliCall {
  readonly caller: CliCaller;
  readonly request: Request;
  readonly id: string | undefined;
}

/** The Caller `resolveCaller` finds for a request: a signed-in device or an Organization Token. */
type CliCaller = Effect.Success<ReturnType<typeof resolveCaller>>;

/** What a CLI route answers: a Response as it is, any other value as JSON. */
export type CliHandler = (call: CliCall) => Effect.Effect<unknown, unknown, AppServices>;

/** CLI routes by key: the method, then the path with its second segment as `:id` (`DELETE tokens/:id`). */
export type CliRoutes = Readonly<Record<string, CliHandler>>;

/** `request`'s JSON body as `schema` decodes it, or a Validation saying `message` when it is no JSON or does not fit. */
export const decodeBody = <S extends Schema.Decoder<unknown>>(schema: S, request: Request, message: string) =>
  decodeJsonBody(schema, request, new Validation({ message, userFacing: true }));

// The CLI reads a bare 404 as an unsupported route, so a missing Volume or run answers as a refusal.
export const missingRefusal = (message: string) => cliRefusalResponse({ code: "not_found", message, details: null });

export const noSuchServer = () => missingRefusal("No such Server.");

/** The Machine ID `:id` names, or null when it is no Machine ID. */
export const serverIdOf = (id: string | undefined) => {
  const machineId = decodeURIComponent(id ?? "");
  return Schema.is(rustMachineIdSchema)(machineId) ? machineId : null;
};

/** `catchTag("NotFound", refusedMissing)`: a NotFound answered as the refusal the CLI reads instead of a bare 404. */
export const refusedMissing = (missing: NotFound) => Effect.succeed(missingRefusal(missing.message));

/**
 * The refusal for a credential that isn't a signed-in person's, or null for one that is. A token can't make or revoke
 * tokens (see `requireSignedIn`): refused before its request is read, so no field of it is blamed.
 */
export const personOnly = (caller: Caller, message: string) =>
  caller.credential.kind === "session"
    ? null
    : cliRefusalResponse({ code: "forbidden", message, details: { next: "ployz login" } });

/** {@link personOnly}, and the refusal for a caller who may not open a shell on Servers, support staff included. */
export const shellPersonOnly = (caller: Caller, message: string) => {
  const refused = personOnly(caller, message);
  if (refused !== null) return refused;
  const lacking = lacks({ ...caller, support: null }, "servers:shell");
  return lacking === null ? null : cliRefusalResponse({ code: "forbidden", message: lacking, details: null });
};

export const approvalHeader = (request: Request) => request.headers.get(APPROVAL_HEADER);

/** Asks a person before the operation runs, unless the request carries the digest they said yes to. */
export const gated = (caller: Caller, request: Request): Gate => (asked) => gateOperation(caller, approvalHeader(request), asked);

/** The refusal for an approval ID an older `ployz` carries, checked before anything that might skip the gate. */
export const approvalCarried = (request: Request) => {
  const older = olderApproval(approvalHeader(request));
  return older === null ? null : cliRefusalResponse(older);
};

export const accepted = (id: string) => Response.json({ id }, { status: 202 });

/** Runs `run` with the request key and approval an operation call carries, or answers the refusal for a malformed key. */
export const keyedCall = <A, E, R>(request: Request, run: (call: OperationCall) => Effect.Effect<A, E, R>) => {
  const key = requestKeyOf(request.headers.get(REQUEST_KEY_HEADER));
  return key.ok ? run({ key: key.key, approved: approvalHeader(request) }) : Effect.succeed(cliRefusalResponse(key.refusal));
};

/** An operation's admission as the CLI reads it: `reply` for the attempt, the refusal, or no such Server when nothing was found. */
export const admitted = <A>(admission: Admission | null, reply: (id: string) => A) => {
  if (admission === null) return noSuchServer();
  return admission.ok ? reply(admission.id) : cliRefusalResponse(admission.refusal);
};
