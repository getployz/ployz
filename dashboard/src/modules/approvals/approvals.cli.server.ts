import { Effect } from "effect";
import { ApprovalDecision } from "#/modules/approvals/approvals";
import { decideAndApply } from "#/modules/approvals/apply.server";
import { freshApproval, freshOperationDigest } from "#/modules/machines/server-operations.server";
import { cliRefusalResponse } from "#/server/public-error";
import { decodeBody, refusedMissing, type CliRoutes } from "#/server/cli-call";

/** Reading an approval a destructive write waits on, and deciding it. */
export const approvalsCliRoutes = {
  "GET approvals/:id": ({ caller, id }) =>
    freshApproval(caller.organization.id, id ?? "").pipe(
      Effect.map((approval) => ({ approval })),
      Effect.catchTag("NotFound", refusedMissing),
    ),
  "POST approvals/:id": ({ caller, request, id }) =>
    Effect.gen(function* () {
      const decision = yield* decodeBody(ApprovalDecision, request, "An approval takes `approve` with the digest you reviewed, or `reject`.");
      return yield* decideAndApply(caller, id ?? "", decision, freshOperationDigest).pipe(
        Effect.map((decided) => decided.ok ? { approval: decided.approval } : cliRefusalResponse(decided.refusal)),
        Effect.catchTag("NotFound", refusedMissing),
      );
    }),
} satisfies CliRoutes;
