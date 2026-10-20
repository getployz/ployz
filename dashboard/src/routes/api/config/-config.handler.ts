import "@tanstack/react-start/server-only";
import { Effect, Option, Schema } from "effect";
import { approvalAsks, approvalCommand, asksApproval } from "#/modules/approvals/approvals";
import { gateOperation, hasApprover, redeemApproval, requestApproval } from "#/modules/approvals/approvals.server";
import { callStore, refusal } from "#/modules/config-store/config-store.server";
import { protectorOnly, unprotectAsked } from "#/modules/config-store/protected.server";
import { cloudStore } from "#/modules/config-store/store-sdk.server";
import { StoreCommand, StoreQuery, type StoreCall } from "#/modules/config-store/store.contract";
import { receiveUpload } from "#/modules/config-store/upload.server";
import { callerEnvironments, callerPermissions } from "#/modules/identity/actor";
import { resolveCaller } from "#/modules/identity/caller.server";
import { lacks } from "#/modules/organization/access.server";
import { readCappedBody } from "#/server/capped-body";
import { NotFound, Validation } from "#/server/public-error";

/** Room for one whole Config file (256 KiB), however JSON escapes it. */
const MAX_REQUEST_BYTES = 4 * 1024 * 1024;

/**
 * `POST /api/config/read` and `/api/config/write`: the Config Store's two public operations, as the calling
 * Organization. `POST /api/config/upload/<Deployment ID>` keeps a gzipped tar of the source a Deployment the CLI
 * admits next builds from. Nothing else of the Store is reachable over HTTPS.
 */
export const handleConfigRequest = Effect.fn("ConfigStore.handle")(function* (request: Request) {
  const operation = new URL(request.url).pathname.replace(/^\/api\/config\//, "");
  const upload = /^upload\/([^/]+)$/.exec(operation)?.[1];
  if (request.method !== "POST" || (operation !== "read" && operation !== "write" && upload === undefined)) {
    return yield* new NotFound({ message: "Not found." });
  }
  // Telemetry only: the CLI names a detected coding agent; nothing else reads it.
  const agent = request.headers.get("x-ployz-agent");
  yield* Effect.annotateCurrentSpan("ployz.agent", agent ?? "none");
  const caller = yield* resolveCaller(request.headers);
  if (upload !== undefined) {
    // A blob lands in no Environment: unreferenced, it's released after a day, and the Deployment that names it meets the Store's Environment check.
    const lacking = lacks(caller, "changes:deploy", "store");
    if (lacking !== null) return refusal({ code: "forbidden", message: lacking, details: { permission: "changes:deploy" } });
    const refused = yield* receiveUpload(yield* cloudStore, caller.organization.id, decodeURIComponent(upload), request.body);
    return refused === undefined
      ? Response.json({ uploaded: decodeURIComponent(upload) }, { headers: { "cache-control": "no-store" } })
      : refusal(refused);
  }
  const body = yield* Effect.tryPromise(() => readCappedBody(request, MAX_REQUEST_BYTES)).pipe(Effect.orElseSucceed(() => new Uint8Array(0)));
  if (body === null) {
    return refusal({ code: "invalid_argument", message: "The request is over 4 MiB.", details: { max_bytes: MAX_REQUEST_BYTES } });
  }
  const input: unknown = yield* Effect.try({
    try: () => JSON.parse(new TextDecoder().decode(body)),
    catch: () => new Validation({ message: "Expected a JSON body.", userFacing: true }),
  });
  const query = operation === "read" ? Option.getOrUndefined(Schema.decodeUnknownOption(StoreQuery)(input)) : undefined;
  const command = operation === "write" ? Option.getOrUndefined(Schema.decodeUnknownOption(StoreCommand)(input)) : undefined;
  const call: StoreCall | undefined = query !== undefined ? { operation: "read", query }
    : command !== undefined ? { operation: "write", command } : undefined;
  if (call === undefined) {
    return refusal({ code: "invalid_argument", message: `Expected a ${operation === "read" ? "query" : "command"}.`, details: null });
  }
  // An approval the write names must be this Caller's, for this command, even where nobody is asked. A retry from
  // `ployz` asks only into a Protected Environment, so it carries the approval it was given there.
  const named = request.headers.get("x-ployz-approval");
  const asks = call.operation === "write" && (asksApproval(caller.via, call.command) || (named !== null && approvalCommand(call.command) === "retry"));
  const trusted = asks
    ? yield* redeemApproval(caller, named, { command: approvalCommand(call.command) })
    : { ok: true, approval: "not_required" } as const;
  if (!trusted.ok) return refusal(trusted.refusal);
  // Unprotecting an Environment from `ployz` or `ployz mcp` waits for a human, as the sidebar does.
  const unprotect = call.operation === "write"
    ? yield* unprotectAsked(caller.organization.id, call.command).pipe(
      Effect.catchTag("StoreRefused", (error) => Effect.succeed({ refused: error.refusal })),
    )
    : null;
  if (unprotect !== null && "refused" in unprotect) return refusal(unprotect.refused);
  if (unprotect?.asked) {
    const refused = protectorOnly(caller, unprotect.asked.preview.environment);
    if (refused !== null) return refusal(refused);
    const gated = yield* gateOperation(caller, named, unprotect.asked);
    if (!gated.ok) return refusal(gated.refusal);
  }
  const approval = asks && (yield* hasApprover(caller.organization.id)) ? trusted.approval : "not_required";
  const gate = { approval, asks: call.operation === "write" ? approvalAsks(caller.via, call.command) : "destroys", permissions: callerPermissions(caller), environments: callerEnvironments(caller) } as const;
  const sent: StoreCall = unprotect === null ? call : { operation: "write", command: unprotect.command };
  const result = yield* callStore(caller.organization.id, caller.userId, sent, { source: "cli", agent }, gate);
  if (result.ok) return Response.json(result.value, { headers: { "cache-control": "no-store" } });
  if (call.operation === "write" && result.refusal.code === "approval_required") {
    return refusal(yield* requestApproval(caller, call.command, result.refusal, named));
  }
  return refusal(result.refusal);
});

