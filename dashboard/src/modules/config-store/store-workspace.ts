import type { ConfigCommand, ConfigWritten, EnvironmentListing, Removal } from "@ployz/sdk";
import { Option, Schema } from "effect";
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
  environment.removal !== null && environment.removal.in_flight;

/** What an Environment has, in a few words, wherever the tree is listed: "default", "deleting". */
export function storeEnvironmentNotes(environment: EnvironmentListing) {
  const removal = environment.removal;
  return [
    environment.default && "default",
    removal && (removing(environment) ? "deleting" : removal.status === "applied" ? "off your servers" : `deletion ${removal.status}`),
  ].filter((note): note is string => typeof note === "string");
}

/** What a deletion removes: one Environment, or a whole Project (`environment` null). */
export type TeardownTarget = { project: string; environment: string | null };

/** Volume names by Environment. */
type ByEnvironment = Readonly<Record<string, readonly string[]>>;

/**
 * What the user confirmed: the Volumes whose data they accepted losing, the version the Store reviewed once it asked
 * for one, and the Volumes they gave up every copy of, wherever one turns up.
 */
export type RemovalConsent = { accept: ByEnvironment; version: string | null; everyCopy: ByEnvironment };

/** A Server the Store couldn't check, and why. */
type Unchecked = { server: string; why: string };

/** Sent, or a question for the user first: what the Store reviewed, and the Servers it couldn't check. */
export type RemovalAnswer = { sent: Removal } | { ask: RemovalConsent; waitingOn: readonly Unchecked[] };

const ByEnvironmentSchema = Schema.Record(Schema.String, Schema.Array(Schema.String));

/** A `confirmation_required` removal refusal: the version the Store reviewed and the Volumes it loses. */
const decodeReviewed = Schema.decodeUnknownOption(Schema.Struct({ version: Schema.String, accept: ByEnvironmentSchema }));

/** An `unavailable` removal refusal: the Servers it couldn't check, and the every-copy consent it offers. */
const decodeUnchecked = Schema.decodeUnknownOption(Schema.Struct({
  waiting_on: Schema.Array(Schema.Struct({ server: Schema.String, why: Schema.String })),
  every_copy: Schema.Struct({
    input: Schema.Struct({ version: Schema.String, accept_volume_loss: ByEnvironmentSchema, every_copy: ByEnvironmentSchema }),
  }),
}));

/** The Volumes as the command carries them. */
function carried(byEnvironment: ByEnvironment): Record<string, string[]> {
  return Object.fromEntries(Object.entries(byEnvironment).map(([environment, volumes]) => [environment, [...volumes]]));
}

type Commit = (command: ConfigCommand, handles: readonly string[]) => Promise<ConfigWritten>;

const byText = (left: string, right: string) => left.localeCompare(right);

/** The same Volumes, by Environment, ignoring order. */
function same(left: ByEnvironment, right: ByEnvironment) {
  const keyed = (byEnvironment: ByEnvironment) => JSON.stringify(Object.entries(byEnvironment)
    .filter(([, volumes]) => volumes.length > 0).sort(([left], [right]) => byText(left, right))
    .map(([environment, volumes]) => [environment, [...volumes].sort(byText)]));
  return keyed(left) === keyed(right);
}

/**
 * Delete `target`, as `ployz env rm` and `ployz project rm` do: one `remove`, which the Store finishes on its own.
 * Without a version the Store names the one it reviewed; when it loses exactly the Volumes the user accepted, that
 * version goes back at once, and anything else is a question for the user. So is a Server that couldn't say whether
 * it holds the data: the Store offers giving up every copy.
 */
export async function removeTarget(commit: Commit, target: TeardownTarget, consent: RemovalConsent): Promise<RemovalAnswer> {
  const written = await commit({
    command: "remove", project: target.project, environment: target.environment, version: consent.version,
    accept_volume_loss: carried(consent.accept), every_copy: carried(consent.everyCopy),
  }, ["confirmation_required", "unavailable"]).catch((error: Error) => error);
  if (!(written instanceof Error)) {
    if (written.written !== "removal") throw new Error(`The Store answered a removal with ${written.written}`);
    return { sent: written };
  }
  if (!(written instanceof StoreRefused)) throw written;
  const reviewed = written.code === "confirmation_required" ? Option.getOrNull(decodeReviewed(written.details)) : null;
  if (reviewed) {
    const asked: RemovalConsent = { ...consent, accept: reviewed.accept, version: reviewed.version };
    return consent.version === null && same(asked.accept, consent.accept) ? removeTarget(commit, target, asked) : { ask: asked, waitingOn: [] };
  }
  const unchecked = written.code === "unavailable" ? Option.getOrNull(decodeUnchecked(written.details)) : null;
  if (!unchecked) throw written;
  const { input } = unchecked.every_copy;
  return {
    ask: { accept: input.accept_volume_loss, version: input.version, everyCopy: input.every_copy },
    waitingOn: unchecked.waiting_on,
  };
}
