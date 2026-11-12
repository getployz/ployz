/**
 * A person's teardown, as this browser asks for it: when a Server of the Cluster enforces Device approval, deleting an
 * Environment, a Project, a Server or a Namespace's leftovers takes off only what their passkey approved. The body is
 * drafted by the ployz-core code the Servers run, every term shows read back from it, and the passkey answers exactly
 * that body. A browser key never signs one: a session lent for deploys says nothing about deleting data.
 *
 * Cloud relays the Servers, the Namespaces and the Volumes. What it relays shows, but each Server checks the body
 * itself, so a teardown that names something else only gets refused there.
 */
import type { MachineId, TeardownApproval, TeardownScope } from "@ployz/sdk";
import { teardownBody } from "@ployz/sdk/authority";
import { Schema } from "effect";
import { shownText } from "#/modules/github/preview-review";
import { clusterOf, type ScopeMachine, utc } from "./approval-session";
import type { Review } from "./approval-prompt";
import { PasskeyRefused, passkeyAnswer, statementChallenge } from "./passkey-statement";

/** What a Server advertises once it takes a teardown. */
export const TEARDOWN_CAPABILITY = "ployz.teardown.v1";

export type TeardownMachine = ScopeMachine & { readonly name: string; readonly capabilities: readonly string[] };

/** What the person deletes, and where. */
export type TeardownAsk = {
  /** Every Server, as this tab last saw them. */
  readonly machines: readonly TeardownMachine[];
  /** The Servers the deletion acts on, which name the Cluster; none picks the only one. */
  readonly near: readonly string[];
  /** The scope the person signs, given every Server of the Cluster. */
  readonly scope: (members: MachineId[]) => TeardownScope;
  /** Each Docker Volume's Volume name, shown beside it. */
  readonly volumes: Readonly<Record<string, string>>;
  /** The review's title and button: "Delete shop/staging?", "Delete". */
  readonly title: string;
  readonly action: string;
};

/** What a browser sends: the passkey's own answer. A browser never signs as a Device. */
export type PasskeyTeardown = Extract<TeardownApproval, { passkey: unknown }>;

/** Why nothing was deleted, in words the dialog shows as they are. */
export class TeardownRefused extends Error {}

const NOTE = "Your servers take off only what you approve here, and your passkey answers for exactly these lines.";

const Terms = Schema.fromJsonString(Schema.Struct({ cluster: Schema.String, issued_at: Schema.Number, expires_at: Schema.Number }));

/** The terms a teardown body holds besides its scope, read back from its own bytes. */
function termLines(body: string) {
  const terms = Schema.decodeUnknownSync(Terms)(body);
  return {
    lines: [
      `  cluster ${shownText(terms.cluster)}`,
      "  signer your passkey",
      `  valid ${shownText(utc(terms.issued_at))} to ${shownText(utc(terms.expires_at))}`,
    ],
    expiresAt: terms.expires_at,
  };
}

/**
 * The teardown of `ask`, answered by the person's passkey once they approve every line of it. Undefined when no Server
 * of the Cluster enforces, so the deletion goes as before. Throws a `TeardownRefused` when the person cancels, the
 * passkey doesn't answer, an enforcing Server can't take a teardown yet, or Cloud can't tell which Cluster it is:
 * nothing is deleted then.
 */
export async function approveTeardown(ask: TeardownAsk, review: (review: Review) => Promise<boolean>, now = Date.now()): Promise<PasskeyTeardown | undefined> {
  const { cluster, members } = clusterOf(ask.near, ask.machines);
  const enforcing = members.filter((machine) => machine.deviceEnforcement === true);
  if (enforcing.length === 0) return undefined;
  const older = enforcing.find((machine) => !machine.capabilities.includes(TEARDOWN_CAPABILITY));
  if (older !== undefined) throw new TeardownRefused(`${older.name} needs a newer Ployz to delete from the dashboard. Upgrade it, then try again. Nothing deleted.`);
  if (cluster === undefined) throw new TeardownRefused("Cloud couldn't check the servers. Try again. Nothing deleted.");
  const servers = Object.fromEntries(ask.machines.map((machine) => [machine.id, machine.name]));
  let drafted: { body: string; lines: string[] };
  try {
    // SAFETY: each id is a Server's own, as the runtime watch reports it; ployz-core parses every one again here.
    const ids = members.map((machine) => machine.id as MachineId);
    drafted = teardownBody({ cluster, scope: ask.scope(ids), issued_at: now, signer: "passkey" },
      { servers, volumes: { ...ask.volumes } });
  } catch (error) {
    throw new TeardownRefused(error instanceof Error ? error.message : "This browser couldn't draft the deletion. Nothing deleted.");
  }
  const terms = termLines(drafted.body);
  if (!(await review({ title: ask.title, lines: [...drafted.lines, ...terms.lines], note: NOTE, action: ask.action }))) {
    throw new TeardownRefused("You didn't approve it. Nothing deleted.");
  }
  if (Date.now() >= terms.expiresAt) throw new TeardownRefused("Your approval expired before you answered. Nothing deleted.");
  try {
    return { passkey: { body: drafted.body, assertion: await passkeyAnswer(await statementChallenge(drafted.body, "workflow")) } };
  } catch (error) {
    if (error instanceof PasskeyRefused) throw new TeardownRefused(error.message);
    if (error instanceof Error && error.name === "NotAllowedError") throw new TeardownRefused("Your passkey didn't answer. Nothing deleted.");
    throw new TeardownRefused("Your browser couldn't use a passkey here. Nothing deleted.");
  }
}

/**
 * The scope of taking Server `leaving` out of its Cluster: it leaves, every other Server of the Cluster forgets it, and
 * the Cluster can no longer reach its copies of `volumes`, by Docker name.
 */
export const departure = (leaving: string, volumes: readonly string[]) => (members: MachineId[]): TeardownScope => ({
  kind: "server",
  // SAFETY: the id is the Server's own, as the runtime watch reports it; ployz-core parses it again as it drafts.
  leaving: leaving as MachineId,
  forgetting: members.filter((member) => member !== leaving),
  volumes: [...volumes],
});
