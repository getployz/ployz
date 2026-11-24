import "@tanstack/react-start/server-only";
import type { ConfigTrusted } from "@ployz/sdk";
import { Effect, Option, Schema } from "effect";
import { approvalAsks, asksApproval } from "#/modules/config-store/store-asks";
import { APPROVAL_HEADER, gateOperation, hasApprover, olderApproval, OWNER_OR_ADMIN } from "#/modules/operations/gate.server";
import { linkDeploy } from "#/modules/deploy-approval/deploy-link.server";
import { callStore } from "#/modules/config-store/config-store.server";
import { asRecord, asString } from "#/lib/json";
import { refusal } from "#/lib/refusal";
import { cliRefusalResponse } from "#/server/public-error";
import { protector, protectorOnly, unprotectAsked } from "#/modules/config-store/protected.server";
import { cloudStore } from "#/modules/config-store/store-sdk.server";
import { isProtectedAsk, StoreCommand, StoreQuery, StoreSealingTargets, type StoreCall } from "#/modules/config-store/store.contract";
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
  // The CLI names a detected coding agent; the span is all that reads it.
  const agent = request.headers.get("x-ployz-agent");
  yield* Effect.annotateCurrentSpan("ployz.agent", agent ?? "none");
  const caller = yield* resolveCaller(request.headers);
  if (upload !== undefined) {
    // A blob lands in no Environment: unreferenced, it's released after a day, and the Deployment that names it meets the Store's Environment check.
    const lacking = lacks(caller, "changes:deploy", "store");
    if (lacking !== null) return cliRefusalResponse({ code: "forbidden", message: lacking, details: { permission: "changes:deploy" } });
    const refused = yield* receiveUpload(yield* cloudStore, caller.organization.id, decodeURIComponent(upload), request.body);
    return refused === undefined
      ? Response.json({ uploaded: decodeURIComponent(upload) }, { headers: { "cache-control": "no-store" } })
      : cliRefusalResponse(refused);
  }
  const body = yield* Effect.tryPromise(() => readCappedBody(request, MAX_REQUEST_BYTES)).pipe(Effect.orElseSucceed(() => new Uint8Array(0)));
  if (body === null) {
    return cliRefusalResponse({ code: "invalid_argument", message: "The request is over 4 MiB.", details: { max_bytes: MAX_REQUEST_BYTES } });
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
    return cliRefusalResponse({ code: "invalid_argument", message: `Expected a ${expected}.`, details: null });
  }
  // Without a server, the CLI hears so before the Store reads anything.
  const sealing = operation === "sealing" ? yield* readClusterSealingToSeal(caller.organization.id) : undefined;
  if (sealing !== undefined && sealing.state !== "ready") return Response.json(sealing, { headers: { "cache-control": "no-store" } });
  // The digest of the plan a person said yes to, when this is the resend after Cloud asked.
  const named = request.headers.get(APPROVAL_HEADER);
  const older = olderApproval(named);
  if (older !== null) return cliRefusalResponse(older);
  // A Deploy this caller can't sign, or a member's into a Protected Environment, waits at a link. A CLI that predates links reads the link in the refusal.
  if (named === null && call.operation === "write" && call.command.command === "admit" && call.command.admit === "deploy") {
    const link = yield* linkDeploy(caller, call.command);
    if (link !== null) {
      return cliRefusalResponse("refused" in link ? link.refused : refusal("conflict", link.linked.message, { proposal_id: link.linked.proposal_id, url: link.linked.url }));
    }
  }
  const asks = call.operation === "write" && asksApproval(caller.via, call.command);
  // Unprotecting an Environment from `ployz` or `ployz mcp` waits for a person's yes.
  const unprotect = call.operation === "write"
    ? yield* unprotectAsked(caller.organization.id, call.command).pipe(
      Effect.catchTag("StoreRefused", (error) => Effect.succeed({ refused: error.refusal })),
    )
    : null;
  if (unprotect !== null && "refused" in unprotect) return cliRefusalResponse(unprotect.refused);
  if (unprotect?.asked) {
    const refused = protectorOnly(caller, unprotect.asked.preview.environment);
    if (refused !== null) return cliRefusalResponse(refused);
    const gated = yield* gateOperation(caller, named, unprotect.asked);
    if (!gated.ok) return cliRefusalResponse(gated.refusal);
  }
  const approval: ConfigTrusted["approval"] = asks && (yield* hasApprover(caller.organization.id)) ? "required" : "not_required";
  // Every write here is a CLI's: a plain-text secret from one that predates sealing is refused with the update to run.
  const gate = (approval: ConfigTrusted["approval"]) => ({
    approval, asks: call.operation === "write" ? approvalAsks(caller.via, call.command) : "destroys", permissions: callerPermissions(caller),
    protector: protector(caller), environments: callerEnvironments(caller), from_cli: true,
  } as const);
  const sent: StoreCall = unprotect === null ? call : { operation: "write", command: unprotect.command };
  let result = yield* callStore(caller.organization.id, caller.userId, sent, gate(approval));
  // The Store asks again for the plan as it stands now. A yes runs only that plan, and never a member's own yes to an ask
  // a Protected Environment makes of an owner or admin, which the Store would take.
  if (!result.ok && result.refusal.code === "approval_required") {
    if (isProtectedAsk(result.refusal)) return cliRefusalResponse(refusal("forbidden", OWNER_OR_ADMIN, { protected: true }));
    const asked = asString(asRecord(result.refusal.details)?.["approval"] ?? null);
    if (named !== null && named === asked) result = yield* callStore(caller.organization.id, caller.userId, sent, gate({ approved: named }));
  }
  if (result.ok) {
    const answer = sealing === undefined ? result.value : { state: "ready", publicKey: sealing.publicKey, targets: result.value };
    return Response.json(answer, { headers: { "cache-control": "no-store" } });
  }
  return cliRefusalResponse(result.refusal);
});

