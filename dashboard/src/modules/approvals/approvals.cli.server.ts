import { Effect } from "effect";
import { ApprovalDecision } from "#/modules/approvals/approvals";
import { decideApproval, getApproval } from "#/modules/approvals/approvals.server";
import { cliRefusalResponse } from "#/server/public-error";
import { decodeBody, refusedMissing, type CliRoutes } from "#/modules/commands/cli-call.server";

/** Reading an approval a destructive write waits on, and deciding it. */
export const approvalsCliRoutes = {
  "GET approvals/:id": ({ caller, id }) =>
    getApproval(caller.organization.id, id ?? "").pipe(
      Effect.map((approval) => ({ approval })),
      Effect.catchTag("NotFound", refusedMissing),
    ),
  "POST approvals/:id": ({ caller, request, id }) =>
    Effect.gen(function* () {
      const decision = yield* decodeBody(ApprovalDecision, request, "An approval takes `approve` with the digest you reviewed, or `reject`.");
      return yield* decideApproval(caller, id ?? "", decision).pipe(
        Effect.map((decided) => decided.ok ? { approval: decided.approval } : cliRefusalResponse(decided.refusal)),
        Effect.catchTag("NotFound", refusedMissing),
      );
    }),
} satisfies CliRoutes;
