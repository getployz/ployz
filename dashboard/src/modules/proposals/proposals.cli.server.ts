import { Effect } from "effect";
import { missingRefusal, type CliRoutes } from "#/modules/commands/cli-call.server";
import { proposalStatus } from "#/modules/proposals/proposals.server";
import { cliRefusalResponse } from "#/server/public-error";

/** A Deploy the CLI can't sign follows its proposal here until a person approves or denies it. Only its asker reads it. */
export const proposalsCliRoutes = {
  "GET proposals/:id": ({ caller, id }) =>
    proposalStatus(caller, id ?? "").pipe(Effect.map((read) =>
      !read.ok ? cliRefusalResponse(read.refusal)
      : read.value.requested_by === caller.userId ? { proposal: read.value }
      : missingRefusal(`No proposal is named ${id}.`))),
} satisfies CliRoutes;
