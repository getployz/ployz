import { afterEach, expect, it, vi } from "vitest";
import * as passkey from "#/modules/approval-session/passkey-statement";
import { approvedKey, KeyNotApproved } from "./approved-key";

const CLUSTER = "NdiV1QY9P3ZyoC82YA/FaBs0zlWTG/CXU1tLfcS0CDg=";
const KEYS = { publicKey: CLUSTER, sealTo: CLUSTER };
const KEY = "tskey-auth-kTEST-notarealkey0123456789";
const ASSERTION = { credential_id: "AQID", authenticator_data: "AQID", client_data_json: "AQID", signature: "AQID" };

afterEach(() => {
  vi.restoreAllMocks();
});

it("asks the passkey to approve exactly the sealed key, which never holds the key in the clear", async () => {
  const challenge = vi.spyOn(passkey, "statementChallenge");
  vi.spyOn(passkey, "passkeyAnswer").mockResolvedValue(ASSERTION);

  const on = await approvedKey(KEYS, KEY, 1_000);

  expect(on.sealed_auth_key).toMatch(/^[A-Za-z0-9+/]+=*$/);
  expect(on.sealed_auth_key).not.toContain(KEY);

  const digest = Buffer.from(await crypto.subtle.digest("SHA-256", Buffer.from(on.sealed_auth_key, "base64"))).toString("hex");
  const body = JSON.stringify({ auth_key: digest, cluster: CLUSTER, expires_at: 901_000, issued_at: 1_000, signer: "passkey" });
  expect(on.approval).toEqual({ passkey: { body, assertion: ASSERTION } });
  expect(challenge).toHaveBeenCalledExactlyOnceWith(body, "workflow");
});

it("sends nothing when the passkey doesn't answer", async () => {
  vi.spyOn(passkey, "passkeyAnswer").mockRejectedValue(Object.assign(new Error("denied"), { name: "NotAllowedError" }));

  await expect(approvedKey(KEYS, KEY)).rejects.toEqual(new KeyNotApproved("Your passkey didn't answer. Try again."));
});
