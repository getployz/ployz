/**
 * A restore or empty of a Server's Trash as the browser approves it: the approval names the Cluster, the one Server and
 * exactly the entries picked, written here by the ployz-core code the Servers check it with, and the person's passkey
 * answers it with a prompt of its own. Cloud only carries it. The Server acts only on the entries the passkey signed for
 * it, so the approval reaches no other Server, entry or action.
 */
import type { QueryClient } from "@tanstack/react-query";
import type { MachineId, Signed, TrashAction } from "@ployz/sdk";
import { Schema } from "effect";
import { trashApprovalBody } from "@ployz/sdk/authority";
import { rustMachineIdSchema } from "#/lib/schema";
import { PasskeyDeclined, signWithPasskey } from "#/modules/passkeys/passkey-statement";
import { freshSealing } from "#/modules/sealing/sealing.queries";

/** Why the approval couldn't be made, in words the page shows as they are. */
export class TrashNotApproved extends Error {}

const UNREAD = "Couldn't read your cluster. Try again.";

/** The person's passkey approval of `action` on exactly `ids` in the Trash of `machine`, in the Cluster `cluster`. */
export async function approveTrash(
  cluster: { readonly publicKey: string },
  machine: MachineId,
  action: TrashAction,
  ids: readonly string[],
  now = Date.now(),
): Promise<Signed> {
  const body = trashApprovalBody({ cluster: cluster.publicKey, machine, action, ids: [...ids], issued_at: now, signer: "passkey" });
  try {
    return await signWithPasskey(body, "workflow");
  } catch (error) {
    if (error instanceof PasskeyDeclined) throw new TrashNotApproved(error.message);
    if (error instanceof Error && error.name === "NotAllowedError") throw new TrashNotApproved("Your passkey didn't answer. Try again.");
    throw new TrashNotApproved("Your browser couldn't use a passkey here.");
  }
}

/** [`approveTrash`] for the Organization's Cluster, read afresh, as JSON text a server function carries. */
export async function approvedTrash(queryClient: QueryClient, organizationSlug: string, machine: string, action: TrashAction, ids: readonly string[]) {
  const fresh = await freshSealing(queryClient, organizationSlug, () => new TrashNotApproved(UNREAD));
  if (fresh.sealing.state !== "ready") throw new TrashNotApproved(UNREAD);
  return JSON.stringify(await approveTrash(fresh.sealing, Schema.decodeUnknownSync(rustMachineIdSchema)(machine), action, ids));
}
