import "@tanstack/react-start/server-only";
import { Effect, Option, Schema } from "effect";
import { approvalAsks, asksApproval } from "#/modules/approvals/approvals";
import { gateOperation, hasApprover, redeemApproval, redeeming, requestApproval } from "#/modules/approvals/approvals.server";
import { callStore, refusal } from "#/modules/config-store/config-store.server";
import { protector, protectorOnly, unprotectAsked } from "#/modules/config-store/protected.server";
import { cloudStore } from "#/modules/config-store/store-sdk.server";
import { StoreCommand, StoreQuery, StoreSealingTargets, type StoreCall } from "#/modules/config-store/store.contract";
import { receiveUpload } from "#/modules/config-store/upload.server";
import { callerEnvironments, callerPermissions } from "#/modules/identity/actor";
import { resolveCaller } from "#/modules/identity/caller.server";
import { lacks } from "#/modules/organization/access.server";
import { readClusterSealingToSeal } from "#/modules/sealing/cluster-sealing.server";
import { readCappedBody } from "#/server/capped-body";
import { NotFound, Validation } from "#/server/public-error";

/** Room for one whole Config file (256 KiB), however JSON escapes it. */
const MAX_REQUEST_BYTES = 4 * 1024 * 1024;

/**
 * `POST /api/config/read` and `/api/config/write`: the Config Store's two public operations, as the calling
 * Organization. `POST /api/config/sealing` tells a CLI whether its Organization seals secrets on the client, and
 * where a write's secrets land once it does. `POST /api/config/upload/<Deployment ID>` keeps a gzipped tar of the
 * source a Deployment the CLI admits next builds from. Nothing else of the Store is reachable over HTTPS.
 */
export const handleConfigRequest = Effect.fn("ConfigStore.handle")(function* (request: Request) {
  const operation = new URL(request.url).pathname.replace(/^\/api\/config\//, "");
  const upload = /^upload\/([^/]+)$/.exec(operation)?.[1];
  if (request.method !== "POST" || (operation !== "read" && operation !== "write" && operation !== "sealing" && upload === undefined)) {
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
  const targets = operation === "sealing" ? Option.getOrUndefined(Schema.decodeUnknownOption(StoreSealingTargets)(input)) : undefined;
  const call: StoreCall | undefined = query !== undefined ? { operation: "read", query }
    : command !== undefined ? { operation: "write", command }
    : targets !== undefined ? { operation: "read", query: { query: "sealing_targets", ...targets } } : undefined;
  if (call === undefined) {
    const expected = operation === "read" ? "query" : operation === "sealing" ? "sealing target" : "command";
    return refusal({ code: "invalid_argument", message: `Expected a ${expected}.`, details: null });
  }
  // Without a server, the CLI hears so before the Store reads anything.
  const sealing = operation === "sealing" ? yield* readClusterSealingToSeal(caller.organization.id) : undefined;
  if (sealing !== undefined && sealing.state !== "ready") return Response.json({ state: sealing.state }, { headers: { "cache-control": "no-store" } });
  // An approval the write names must be this Caller's, for this command, even where nobody is asked.
  const named = request.headers.get("x-ployz-approval");
  const asks = call.operation === "write" && asksApproval(caller.via, call.command);
  // Any write that carries one redeems it, even one that would ask no one or is already done.
  const trusted = call.operation === "write" && (asks || named !== null)
    ? yield* redeemApproval(caller, named, yield* redeeming(caller.organization.id, call.command, named))
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
  // Every write here is a CLI's: a plain-text secret from one that predates sealing is refused with the update to run.
  const gate = {
    approval, asks: call.operation === "write" ? approvalAsks(caller.via, call.command) : "destroys", permissions: callerPermissions(caller),
    protector: protector(caller), environments: callerEnvironments(caller), from_cli: true,
  } as const;
  const sent: StoreCall = unprotect === null ? call : { operation: "write", command: unprotect.command };
  const result = yield* callStore(caller.organization.id, caller.userId, sent, { source: "cli", agent }, gate);
  if (result.ok) {
    const answer = sealing === undefined ? result.value : { state: "ready", publicKey: sealing.publicKey, targets: result.value };
    return Response.json(answer, { headers: { "cache-control": "no-store" } });
  }
  if (call.operation === "write" && result.refusal.code === "approval_required") {
    return refusal(yield* requestApproval(caller, call.command, result.refusal, named));
  }
  return refusal(result.refusal);
});

