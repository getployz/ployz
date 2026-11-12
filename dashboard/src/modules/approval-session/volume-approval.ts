/**
 * A Volume workflow as the browser approves it on Servers that enforce device approval: one Move, Mirror, Mirror removal,
 * Restore, resize or seed, drafted here by the ployz-core code the CLI and the Servers run, shown line by line from the
 * bytes the passkey answers, then answered by the person's passkey with a prompt of its own. A browser key under a session
 * grant never answers one. Cloud relays the Volume's names and Servers and stores the answer; every Server checks it
 * against its own records.
 */
import type { MachineId, OperationId, ResolvedServiceSpec, Signed } from "@ployz/sdk";
import type { DraftWorkflow, VolumeDraft } from "@ployz/sdk/authority";
import { volumeApprovalBody } from "@ployz/sdk/authority";
import type { Member } from "#/modules/volume-run/volume-run";
import { reviewApproval } from "./approval-prompt";
import { PasskeyRefused, signWithPasskey } from "./passkey-statement";

/**
 * The Volume a workflow acts on, as Cloud relays it: its names, its Cluster's public key, and what each of its Servers
 * answered just now, with whether it enforces device approval and takes a passkey's own answer to a Volume workflow.
 */
export type VolumeScope = {
  readonly cluster: string;
  readonly namespace: string;
  readonly dockerVolume: string;
  /** The Volume's name in its Environment. */
  readonly name: string;
  readonly members: readonly Member[];
};

/** One Volume workflow to approve, with the words its review shows. */
export type VolumeChange = {
  /** The review's title, naming the change, such as "Move data to web-2". */
  readonly title: string;
  /** The review's button. */
  readonly action: string;
  readonly workflow: DraftWorkflow;
  /** Every Server the workflow names. */
  readonly parties: readonly MachineId[];
  /** The command that runs the same change from an approved device, for a Server that can't take this. */
  readonly command: string;
};

/** What the person approved: the answered statement, for a Move the spec its target starts, and the lines they read. */
export type VolumeApproved = { readonly approval: Signed; readonly service?: ResolvedServiceSpec; readonly lines: readonly string[] };

/** Why a Volume workflow couldn't be approved, in words shown as they are. Nothing was sent. */
export class VolumeNotApproved extends Error {}

type Party = { readonly id: MachineId; readonly name: string; readonly member: Extract<Member, { answered: true }> | null };

const partiesOf = (members: readonly Member[], ids: readonly MachineId[]): Party[] => ids.map((id) => {
  const member = members.find((each) => each.machine.id === id);
  return { id, name: member?.machine.name ?? id, member: member?.answered === true ? member : null };
});

/**
 * Whether a Server `change` names enforces device approval, so it runs only with an approval. With none heard to, the
 * run asks as before, and its plan says why it can't reach a Server that didn't answer.
 */
export const enforcedOn = (members: readonly Member[], parties: readonly MachineId[]) =>
  partiesOf(members, parties).some((party) => party.member?.enforces === true);

/** An operation ID as `OperationId::random` makes one: 32 lowercase hex characters. */
// SAFETY: a UUID without its dashes is 32 lowercase hex characters, the form OperationId parses.
const operationId = () => crypto.randomUUID().replaceAll("-", "") as OperationId;

/**
 * The approval of `change` on `scope`, after the person reads every line and their passkey answers once. Undefined when
 * no Server it names enforces, so the change runs as before; null when the person cancels. Throws `VolumeNotApproved`,
 * before any prompt, for a Server that can't take a passkey's answer, and after it when the passkey doesn't answer.
 */
export async function approveVolume(scope: VolumeScope, change: VolumeChange, now = Date.now()): Promise<VolumeApproved | null | undefined> {
  if (!enforcedOn(scope.members, change.parties)) return undefined;
  const parties = partiesOf(scope.members, change.parties);
  const unheard = parties.find((party) => party.member === null);
  if (unheard !== undefined) {
    throw new VolumeNotApproved(`${unheard.name} didn't answer, so this can't be approved here. Try again, or run ${change.command} from an approved device.`);
  }
  const older = parties.find((party) => party.member?.enforces === true && party.member.passkey !== true);
  if (older !== undefined) {
    throw new VolumeNotApproved(`${older.name} needs a newer Ployz to approve this from the dashboard. Upgrade it, or run ${change.command} from an approved device.`);
  }
  const draft = draftVolume(scope, change, now);
  const approved = await reviewApproval({
    title: change.title,
    lines: draft.lines,
    note: "Your servers run this only with your passkey. Read what it approves first.",
    action: change.action,
  });
  if (!approved) return null;
  let approval: Signed;
  try {
    approval = await signWithPasskey(draft.body, "volume");
  } catch (error) {
    if (error instanceof PasskeyRefused) throw new VolumeNotApproved(error.message);
    if (error instanceof Error && error.name === "NotAllowedError") {
      throw new VolumeNotApproved("No passkey answered. If this browser has none, add one under Account, then approve it on your servers. Nothing changed.");
    }
    throw new VolumeNotApproved("Your browser couldn't use a passkey here. Nothing changed.");
  }
  return { approval, ...(draft.service !== undefined && { service: draft.service }), lines: draft.lines };
}

/** The approval `change` describes at `now`, drafted by ployz-core: the body the passkey answers and every line it shows. */
export function draftVolume(scope: VolumeScope, change: VolumeChange, now = Date.now()): VolumeDraft {
  try {
    return volumeApprovalBody({
      cluster: scope.cluster,
      operation: operationId(),
      namespace: scope.namespace,
      volume: scope.dockerVolume,
      name: scope.name,
      servers: Object.fromEntries(partiesOf(scope.members, change.parties).map((party) => [party.id, party.name])),
      workflow: change.workflow,
      issued_at: now,
      signer: "passkey",
    });
  } catch (error) {
    throw new VolumeNotApproved(error instanceof Error ? error.message : "This change can't be approved here.");
  }
}
