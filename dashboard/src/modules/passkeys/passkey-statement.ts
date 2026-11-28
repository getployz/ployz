/**
 * A passkey's own answer to one statement. The browser writes the statement's canonical body, and the person's passkey
 * answers a challenge that is the digest of its signing input, so the answer covers that body and nothing else. No
 * browser key is lent and nothing outlives the prompt.
 *
 * Servers take such an answer for an automatic deploy rule, which stands for every future push, for one commit a
 * preview builds, and for a Private Access approval, which names the one sealed key a router may join a tailnet with. Every passkey prompt in the
 * dashboard goes through `passkeyAnswer`.
 */
import type { PasskeyAssertion, Signed } from "@ployz/sdk";
import { base64url } from "#/lib/bytes";

/** What a signature is for, as `keys::Purpose` names it. Each purpose signs under its own prefix. */
export type Purpose = "workflow" | "rule" | "commit";

const encoder = new TextEncoder();

/** The bytes a key signs, and a passkey's challenge hashes: `ployz-authority/v1`, the purpose, and the body. */
export const signingInput = (body: string, purpose: Purpose = "workflow") =>
  encoder.encode(`ployz-authority/v1\n${purpose}\n${body}`);

/** The challenge a passkey answers for a statement of `purpose`: the SHA-256 of its signing input. */
export const statementChallenge = async (body: string, purpose: Purpose) =>
  new Uint8Array(await crypto.subtle.digest("SHA-256", signingInput(body, purpose)));

/** Why a passkey didn't answer, in words a prompt shows as they are. */
export class PasskeyDeclined extends Error {}

const fromBase64url = (text: string) => {
  const binary = atob(text.replace(/-/g, "+").replace(/_/g, "/"));
  return Uint8Array.from(binary, (character) => character.charCodeAt(0));
};

/** The one passkey a prompt may use: its credential, and its code to name it if another answers. */
export type OnlyPasskey = { readonly credentialId: string; readonly code: string };

/** One user-verified passkey prompt answering `challenge`, as Servers check it, offering only `only` when given. */
export async function passkeyAnswer(challenge: Uint8Array<ArrayBuffer>, only?: readonly OnlyPasskey[]): Promise<PasskeyAssertion> {
  const allowCredentials = only?.map((passkey) => ({ type: "public-key" as const, id: fromBase64url(passkey.credentialId) }));
  // SAFETY: a `publicKey` get request answers with a public key credential, or null.
  const credential = await navigator.credentials.get({
    publicKey: { challenge, rpId: location.hostname, userVerification: "required", timeout: 60_000, allowCredentials },
  }) as PublicKeyCredential | null;
  if (credential === null) throw new PasskeyDeclined("Your passkey didn't answer. Try again.");
  if (only !== undefined && !only.some((passkey) => base64url(credential.rawId) === passkey.credentialId)) {
    throw new PasskeyDeclined(`Use passkey ${only.map((passkey) => passkey.code).join(" or ")}. The one you used isn't trusted here.`);
  }
  // SAFETY: a `publicKey` get request answers with an assertion response.
  const response = credential.response as AuthenticatorAssertionResponse;
  return {
    credential_id: base64url(credential.rawId),
    authenticator_data: base64url(response.authenticatorData),
    client_data_json: base64url(response.clientDataJSON),
    signature: base64url(response.signature),
  };
}

/**
 * `body`, a statement of `purpose` in canonical form, answered by the person's passkey with a prompt of its own. The
 * envelope carries no signature and no grant: the answer is the approval.
 */
export async function signWithPasskey(body: string, purpose: Purpose, only?: readonly OnlyPasskey[]): Promise<Signed> {
  return { body, signatures: [], assertions: [await passkeyAnswer(await statementChallenge(body, purpose), only)] };
}
