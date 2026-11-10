/**
 * Turning Private Access on as the browser does it: the auth key is sealed to the Cluster here, and when a Server
 * enforces Device approval the person's passkey answers an approval of that sealed key with a prompt of its own. Cloud
 * relays both. It never sees the key, and an enforcing Server places a router only for the sealed key the passkey
 * approved, so a Cloud that hides enforcement only gets the key refused.
 */
import type { QueryClient } from "@tanstack/react-query";
import type { PrivateAccessOn } from "@ployz/sdk";
import { privateAccessApprovalBody } from "@ployz/sdk/authority";
import { sealingQueryOptions } from "#/modules/sealing/sealing.queries";
import { privateAccessQueryOptions } from "./private-access.queries";
import { PasskeyRefused, signWithPasskey } from "#/modules/approval-session/passkey-statement";
import { SealError } from "#/modules/sealing/seal-error";
import { isUnauthorized } from "#/lib/error-message";
import { sealPlatform } from "#/modules/variables/seal-value";

const UNSEALED = "Couldn't seal the key, so nothing was sent. Try again.";

/** Why the key couldn't be sealed or approved, in words the form shows as they are. */
export class KeyNotApproved extends Error {}

/**
 * `authKey` sealed to the Organization's Cluster, both read afresh, with the person's passkey approval of that sealed key
 * when a Server enforces. An unread observation asks the passkey, since an enforcing Server needs it.
 */
export async function sealAndApprove(queryClient: QueryClient, organizationSlug: string, authKey: string): Promise<PrivateAccessOn> {
  const [fresh, observed] = await Promise.all([
    queryClient.fetchQuery({ ...sealingQueryOptions(organizationSlug), staleTime: 0 }),
    queryClient.fetchQuery({ ...privateAccessQueryOptions(organizationSlug), staleTime: 0 }).catch(() => null),
  ]).catch((error) => {
    throw isUnauthorized(error) ? error : new KeyNotApproved(UNSEALED);
  });
  if (fresh.sealing.state !== "ready") throw new KeyNotApproved("Add a server to turn private access on.");
  return approvedKey(fresh.sealing.publicKey, authKey, observed?.enforced ?? true);
}

/** `authKey` sealed to the Cluster key `publicKey`, with the person's passkey approval of exactly that sealed key when `enforced`. */
export async function approvedKey(publicKey: string, authKey: string, enforced: boolean, now = Date.now()): Promise<PrivateAccessOn> {
  let sealed: string;
  try {
    sealed = await sealPlatform(publicKey, "private-access", authKey.trim());
  } catch (error) {
    throw new KeyNotApproved(error instanceof SealError ? error.message : UNSEALED);
  }
  if (!enforced) return { sealed_auth_key: sealed };
  const body = privateAccessApprovalBody({ cluster: publicKey, sealed_auth_key: sealed, issued_at: now, signer: "passkey" });
  try {
    return { sealed_auth_key: sealed, approval: await signWithPasskey(body, "workflow") };
  } catch (error) {
    if (error instanceof PasskeyRefused) throw new KeyNotApproved(error.message);
    if (error instanceof Error && error.name === "NotAllowedError") throw new KeyNotApproved("Your passkey didn't answer. Try again.");
    throw new KeyNotApproved("Your browser couldn't use a passkey here.");
  }
}
