import type { ConfigCommand, ConfigWritten, EnvironmentListing } from "@ployz/sdk";
import { isInFlight } from "./store-deployments";
import { StoreRefused } from "./store.contract";

/** A Project's Environments in tree order: roots first, each Branch right under its Parent, siblings as listed. */
export function storeEnvironmentTree(environments: readonly EnvironmentListing[]) {
  const tree: { environment: EnvironmentListing; depth: number }[] = [];
  const visit = (parent: string | null, depth: number) => {
    for (const environment of environments) {
      // A Branch whose Parent isn't listed reads as a root.
      const under = environment.parent !== null && environments.some((row) => row.name === environment.parent) ? environment.parent : null;
      if (under !== parent) continue;
      tree.push({ environment, depth });
      visit(environment.name, depth + 1);
    }
  };
  visit(null, 0);
  return tree;
}

/** A removal from the Servers that hasn't ended. */
export const removing = (environment: EnvironmentListing) =>
  environment.removal !== null && isInFlight(environment.removal.status);

/** What an Environment has, in a few words, wherever the tree is listed: "default", "deleting". */
export function storeEnvironmentNotes(environment: EnvironmentListing) {
  const removal = environment.removal;
  return [
    environment.default && "default",
    removal && (removing(environment) ? "deleting" : removal.status === "applied" ? "off your servers" : `deletion ${removal.status}`),
  ].filter((note): note is string => typeof note === "string");
}

/** What a teardown deletes: one Environment, or a whole Project (`environment` null). */
export type TeardownTarget = { project: string; environment: string | null };

/** The Volumes whose data the user accepted losing, by Environment: each removal accepts only its own. */
export type AcceptedLoss = Readonly<Record<string, readonly string[]>>;

/** Done, or waiting on the Deployment taking `environment` off the Servers. */
export type TeardownStep = { done: true } | { done: false; environment: string; deployment: string };

type Commit = (command: ConfigCommand, handles: readonly string[]) => Promise<ConfigWritten>;

/**
 * One step of the one teardown path, as `ployz env rm` and `ployz project rm` take it. The Store deletes what never ran
 * on the Servers at once; otherwise it names the Environment still there, which a removal Deployment takes off (unless
 * one is already running), and the same step, taken again once that applied, goes on.
 */
export async function teardownStep(commit: Commit, target: TeardownTarget, accepted: AcceptedLoss): Promise<TeardownStep> {
  const remove: ConfigCommand = target.environment === null
    ? { command: "remove_project", project: target.project }
    : { command: "remove_environment", environment: { project: target.project, environment: target.environment } };
  // A closed Branch goes on its own once its removal applied (see `close` below): gone is done.
  const written = await commit(remove, target.environment === null ? [] : ["not_found"]).catch((error: Error) => {
    if (target.environment !== null && error instanceof StoreRefused && error.code === "not_found") return null;
    throw error;
  });
  if (written === null) return { done: true };
  if (written.written !== "environment_removed" && written.written !== "project_removed") {
    throw new Error(`The Store answered a removal with ${written.written}`);
  }
  if (written.teardown === "removed") return { done: true };
  if (written.teardown === "waiting") return { done: false, environment: written.environment, deployment: written.deployment };
  const id = crypto.randomUUID();
  await commit({
    command: "admit", admit: "remove", id, environment: { project: target.project, environment: written.environment },
    version: null, accept_volume_loss: [...accepted[written.environment] ?? []],
    // Deleting one Environment: the Store finishes a Branch itself once this applied, even if this tab is gone.
    close: target.environment !== null,
  }, ["confirmation_required", "invalid_argument"]);
  return { done: false, environment: written.environment, deployment: id };
}
