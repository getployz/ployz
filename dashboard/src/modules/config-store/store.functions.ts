import { getRequest, setResponseHeader } from "@tanstack/react-start/server";
import { createServerFn } from "@tanstack/react-start";
import { Effect } from "effect";
import { refusal } from "#/lib/refusal";
import { linkDeploy } from "#/modules/deploy-approval/deploy-link.server";
import { OWNER_OR_ADMIN } from "#/modules/operations/gate.server";
import { dashboardCaller } from "#/modules/identity/caller.server";
import { callStoreAsMember, writeStoreAsMember } from "./config-store.server";
import { isProtectedAsk, storeReadInput, storeWriteInput } from "./store.contract";
import { authenticated, runActor, strictValidator } from "#/server/tanstack";

/** One bounded Store view. Read it through `store-view.queries.ts`, never directly. */
export const readStoreViewServerFn = createServerFn({ method: "GET" })
  .middleware(authenticated)
  .validator(strictValidator(storeReadInput))
  .handler(async ({ context, data }) => {
    setResponseHeader("cache-control", "private, no-store");
    return await runActor(context, callStoreAsMember(context.actor, data.organizationSlug, { operation: "read", query: data.query }));
  });

/**
 * One Store command, committed or refused. Write through `store-write.ts`, never directly. A member's Deploy into a
 * Protected Environment they can't protect becomes a proposal an owner or admin decides; any other write there is theirs.
 */
export const writeStoreServerFn = createServerFn({ method: "POST" })
  .middleware(authenticated)
  .validator(strictValidator(storeWriteInput))
  .handler(async ({ context, data }) => await runActor(context, Effect.gen(function* () {
    const result = yield* writeStoreAsMember(context.actor, data.organizationSlug, data.command, data.shows);
    if (result.ok || !isProtectedAsk(result.refusal)) return result;
    const { command } = data;
    if (command.command === "admit" && command.admit === "deploy") {
      const link = yield* linkDeploy(yield* dashboardCaller(getRequest().headers, data.organizationSlug), command);
      if (link !== null && "refused" in link) return { ok: false, refusal: link.refused } as const;
      if (link !== null) {
        const { message, proposal_id, url } = link.linked;
        return { ok: false, refusal: { code: "approval_required", message, details: { protected: true, proposal_id, url } } } as const;
      }
    }
    return { ok: false, refusal: refusal("forbidden", OWNER_OR_ADMIN, { protected: true }) } as const;
  })));
