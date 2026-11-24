import { Effect } from "effect";
import { missingRefusal, type CliRoutes } from "#/modules/commands/cli-call.server";
import { askedDeploy } from "#/modules/proposals/proposals.server";

/** A Deploy the CLI can't sign follows its proposal here until a person approves or denies it. Only the credential that asked reads it. */
export const proposalsCliRoutes = {
  "GET proposals/:id": ({ caller, id }) =>
    askedDeploy(caller, id ?? "").pipe(Effect.map((proposal) =>
      proposal === undefined ? missingRefusal(`No proposal is named ${id}.`) : { proposal })),
} satisfies CliRoutes;
