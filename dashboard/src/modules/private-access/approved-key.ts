/**
 * Turning Private Access on as the browser does it: the auth key is sealed to the Cluster here, and the person's passkey
 * answers an approval of that sealed key with a prompt of its own. Cloud relays both. It never sees the key, and a
 * Server places a router only for the sealed key the passkey approved.
 */
import type { QueryClient } from "@tanstack/react-query";
import type { KeyApproval, PrivateAccessOn } from "@ployz/sdk";
import { privateAccessApprovalBody } from "@ployz/sdk/authority";
import { sealingQueryOptions } from "#/modules/sealing/sealing.queries";
import { PasskeyRefused, passkeyAnswer, statementChallenge } from "#/modules/approval-session/passkey-statement";
import { SealError } from "#/modules/sealing/seal-error";
import { isUnauthorized } from "#/lib/error-message";
import { sealPlatform } from "#/modules/variables/seal-value";

const UNSEALED = "Couldn't seal the key, so nothing was sent. Try again.";

/** What a browser sends: the sealed key, with its passkey's answer. A browser never signs as a Device. */
export type BrowserOn = Omit<PrivateAccessOn, "approval"> & { approval?: Extract<KeyApproval, { passkey: unknown }> };

/** Why the key couldn't be sealed or approved, in words the form shows as they are. */
export class KeyNotApproved extends Error {}

/** `authKey` sealed to the Organization's Cluster, read afresh, with the person's passkey approval of that sealed key. */
export async function sealAndApprove(queryClient: QueryClient, organizationSlug: string, authKey: string): Promise<BrowserOn> {
  const fresh = await queryClient.fetchQuery({ ...sealingQueryOptions(organizationSlug), staleTime: 0 }).catch((error) => {
    throw isUnauthorized(error) ? error : new KeyNotApproved(UNSEALED);
  });
  if (fresh.sealing.state !== "ready") throw new KeyNotApproved("Add a server to turn private access on.");
  return approvedKey(fresh.sealing, authKey);
}

/**
 * `authKey` sealed for the Cluster whose identity is `cluster.publicKey`, to `cluster.sealTo`, its recipient key once it
 * activated, with the person's passkey approval of exactly that sealed key.
 */
export async function approvedKey(
  cluster: { readonly publicKey: string; readonly sealTo: string },
  authKey: string,
  now = Date.now(),
): Promise<BrowserOn> {
  const { publicKey } = cluster;
  let sealed: string;
  try {
    sealed = await sealPlatform(cluster, "private-access", authKey.trim());
  } catch (error) {
    throw new KeyNotApproved(error instanceof SealError ? error.message : UNSEALED);
  }
  const body = privateAccessApprovalBody({ cluster: publicKey, sealed_auth_key: sealed, issued_at: now, signer: "passkey" });
  try {
    return { sealed_auth_key: sealed, approval: { passkey: { body, assertion: await passkeyAnswer(await statementChallenge(body, "workflow")) } } };
  } catch (error) {
    if (error instanceof PasskeyRefused) throw new KeyNotApproved(error.message);
    if (error instanceof Error && error.name === "NotAllowedError") throw new KeyNotApproved("Your passkey didn't answer. Try again.");
    throw new KeyNotApproved("Your browser couldn't use a passkey here.");
  }
}
