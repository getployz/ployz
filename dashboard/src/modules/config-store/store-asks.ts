import type { Asks, ConfigCommand } from "@ployz/sdk";
import type { Caller } from "#/modules/identity/actor";

const agentShips = (via: Caller["via"], command: ConfigCommand) =>
  via === "agent" && command.command === "admit" && (command.admit === "deploy" || command.admit === "retry");

/**
 * Cloud can't tell a person running `ployz` from an agent running it, so only a dashboard confirm stands as the yes.
 * A retry counts, since a shutdown's retry or a rollback takes Services off, and so does a cancel, which a member asks
 * an admin for in a Protected Environment. The Store decides which of them ask.
 */
const askedRemoval = (via: Caller["via"], command: ConfigCommand) =>
  via !== "dashboard" && (command.command === "remove" || command.command === "cancel"
    || (command.command === "admit" && (command.admit === "shut_down" || command.admit === "retry")));

/**
 * The writes that may need a person's yes: a Save (the Store's publish), a manual Deploy, a Sync or Take (into a
 * Protected Environment), a retry, a cancel, and a Project or Environment removal or shutdown from `ployz` or an agent.
 */
export function asksApproval(via: Caller["via"], command: ConfigCommand) {
  return command.command === "publish" || (command.command === "admit" && command.admit === "deploy")
    || command.command === "sync" || command.command === "take"
    || agentShips(via, command) || askedRemoval(via, command);
}

/** Which Deploys need a yes: every Deploy or retry an agent asks for, else only a Deploy that destroys something. */
export function approvalAsks(via: Caller["via"], command: ConfigCommand): Asks {
  return agentShips(via, command) ? "every_deploy" : "destroys";
}
