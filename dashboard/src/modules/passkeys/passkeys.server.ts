import "@tanstack/react-start/server-only";
import type { MachineDetails, TrustedPasskey } from "@ployz/sdk";
import {
  generateRegistrationOptions,
  type RegistrationResponseJSON,
  verifyRegistrationResponse,
} from "@simplewebauthn/server";
import { and, desc, eq, gt, sql } from "drizzle-orm";
import { Data, Effect } from "effect";
import type { Actor } from "#/modules/identity/actor";
import { user, verification } from "#/modules/identity/tables";
import { loadOrganizationConnections, SERVER_REACH_TIMEOUT_MS } from "#/modules/machines/connections.server";
import { requireAccess } from "#/modules/organization/access.server";
import { codeOfPasskeyKey, passkeyKeyOf, type PasskeyKey } from "#/modules/passkeys/passkey-key";
import { passkeyRequest } from "#/modules/passkeys/tables";
import { clusterRead, firstRuntimeFrame } from "#/modules/runtime/organization-runtime.server";
import { Ployz } from "#/modules/runtime/ployz.server";
import { AppConfig } from "#/server/config.server";
import { Database } from "#/server/database.server";

/** Why a registration did not produce a passkey request. The browser shows the message as it is. */
export class PasskeyRefused extends Data.TaggedError("PasskeyRefused")<{ readonly message: string }> {
  readonly publicErrorCategory = "conflict" as const;
  readonly userFacing = true;
}

const CHALLENGE_TTL_MS = 5 * 60_000;
const challengeKey = (userId: string) => `passkey-registration:${userId}`;

/** The relying party a page at the Cloud URL registers for: its host, and its origin as the browser reports it. */
export const relyingPartyOf = (cloud: URL) => ({ id: cloud.hostname, origin: cloud.origin });

const member = Effect.fn("Passkeys.member")(function* (actor: Actor, organizationSlug: string) {
  const access = yield* requireAccess(actor, organizationSlug, "servers:read");
  if (access.kind !== "member") return yield* new PasskeyRefused({ message: "Only a member of the Organization can add a passkey here." });
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select({ email: user.email, name: user.name }).from(user).where(eq(user.id, actor.userId)).limit(1);
  if (row === undefined) return yield* new PasskeyRefused({ message: "Sign in again to add a passkey." });
  return { ...row, organizationId: access.organization.id };
});

/**
 * WebAuthn creation options for a new passkey that answers only to a verified user, kept on the device. The challenge
 * waits five minutes, one per user, so a second "Add passkey" replaces the first.
 */
export const beginPasskeyRegistration = Effect.fn("Passkeys.begin")(function* (actor: Actor, organizationSlug: string) {
  const person = yield* member(actor, organizationSlug);
  const rp = relyingPartyOf((yield* AppConfig).app.url);
  const options = yield* Effect.promise(() => generateRegistrationOptions({
    rpName: "Ployz",
    rpID: rp.id,
    userName: person.email,
    userDisplayName: person.name,
    attestationType: "none",
    authenticatorSelection: { residentKey: "required", userVerification: "required" },
    supportedAlgorithmIDs: [-8, -7],
  }));
  const { drizzle } = yield* Database;
  yield* drizzle.delete(verification).where(eq(verification.identifier, challengeKey(actor.userId)));
  yield* drizzle.insert(verification).values({
    identifier: challengeKey(actor.userId),
    value: options.challenge,
    expiresAt: new Date(Date.now() + CHALLENGE_TTL_MS),
  });
  return options;
});

/**
 * Checks the browser's answer against the waiting challenge, the Cloud origin and its relying party, with the user
 * verified, then files the passkey under the code its key hashes to. Filing proves nothing to a Server: only
 * `ployz device approve --passkey <code>` there makes the passkey trusted.
 */
export const finishPasskeyRegistration = Effect.fn("Passkeys.finish")(function* (actor: Actor, organizationSlug: string, responseJson: string) {
  const person = yield* member(actor, organizationSlug);
  const rp = relyingPartyOf((yield* AppConfig).app.url);
  const { drizzle } = yield* Database;
  const [challenge] = yield* drizzle.delete(verification)
    .where(and(eq(verification.identifier, challengeKey(actor.userId)), gt(verification.expiresAt, sql`now()`)))
    .returning({ value: verification.value });
  if (challenge === undefined) return yield* new PasskeyRefused({ message: "That took too long. Add the passkey again." });
  // SAFETY: the verifier reads every field it needs from the parsed answer and throws on any it can't use.
  const verified = yield* Effect.tryPromise(() => verifyRegistrationResponse({
    response: JSON.parse(responseJson) as RegistrationResponseJSON,
    expectedChallenge: challenge.value,
    expectedOrigin: rp.origin,
    expectedRPID: rp.id,
    requireUserVerification: true,
    supportedAlgorithmIDs: [-8, -7],
  })).pipe(Effect.mapError(() => new PasskeyRefused({ message: "Your browser's answer didn't check out. Add the passkey again." })));
  if (!verified.verified) return yield* new PasskeyRefused({ message: "Your browser's answer didn't check out. Add the passkey again." });
  const credential = verified.registrationInfo.credential;
  const key = passkeyKeyOf(credential.publicKey);
  if (key === null) return yield* new PasskeyRefused({ message: "This passkey uses a kind of key Ployz servers can't check. Try another authenticator." });
  yield* drizzle.insert(passkeyRequest).values({
    organizationId: person.organizationId,
    code: key.code,
    userId: actor.userId,
    credentialId: credential.id,
    algorithm: key.algorithm,
    publicKey: key.key,
    account: person.email,
    expiresAt: sql`now() + interval '24 hours'`,
  }).onConflictDoNothing();
  return { code: key.code };
});

/** The Organization's passkey at `code` while the CLI may still fetch it, in the shape `ployz device approve --passkey` reads. */
export const readPasskeyRequest = Effect.fn("Passkeys.readRequest")(function* (organizationId: string, code: string) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select().from(passkeyRequest)
    .where(and(eq(passkeyRequest.organizationId, organizationId), eq(passkeyRequest.code, code), gt(passkeyRequest.expiresAt, sql`now()`)))
    .limit(1);
  if (row === undefined) return null;
  // SAFETY: the passkey_request_algorithm check admits only these two names.
  return {
    code: row.code,
    credential_id: row.credentialId,
    public_key: { algorithm: row.algorithm as PasskeyKey["algorithm"], key: row.publicKey },
    account: row.account,
    expires_at: row.expiresAt.toISOString(),
  };
});

/**
 * Who approved a passkey, as one Server reports it, or null while that Server doesn't trust it. `server` means on that
 * Server's own terminal; `trusted` covers a call routed to it, and an approver a newer daemon names.
 */
export type PasskeyApproval =
  | { readonly by: "device"; readonly code: string }
  | { readonly by: "server" | "trusted" }
  | null;

/** What one Server says of a passkey; `unanswered` when Cloud couldn't read it. */
export type ServerTrust = { readonly server: string; readonly approval: PasskeyApproval | "unanswered" };

/**
 * The passkeys this person added in the Organization, newest first, each with what every Server says about it. Cloud's
 * own rows never mark a passkey approved, so a passkey is trusted only on the Servers that list it.
 */
export const listPasskeys = Effect.fn("Passkeys.list")(function* (actor: Actor, organizationSlug: string) {
  const { organizationId } = yield* member(actor, organizationSlug);
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.select({ code: passkeyRequest.code, createdAt: passkeyRequest.createdAt, account: passkeyRequest.account })
    .from(passkeyRequest)
    .where(and(eq(passkeyRequest.organizationId, organizationId), eq(passkeyRequest.userId, actor.userId)))
    .orderBy(desc(passkeyRequest.createdAt));
  const servers = rows.length === 0 ? [] : yield* trustedPasskeys(organizationId);
  return {
    observed: servers.some((server) => server.passkeys !== null),
    passkeys: rows.map((row) => ({
      code: row.code,
      account: row.account,
      createdAt: row.createdAt.toISOString(),
      servers: servers.map(({ server, passkeys }): ServerTrust =>
        ({ server, approval: passkeys === null ? "unanswered" : approvalOf(passkeys, row.code) })),
    })),
  };
});

export const approvalOf = (passkeys: readonly { code: string; trusted: TrustedPasskey }[], code: string): PasskeyApproval => {
  const held = passkeys.find((passkey) => passkey.code === code);
  if (held === undefined) return null;
  const by = held.trusted.approved_by;
  if (by.by === "device") return { by: "device", code: by.code };
  return { by: by.by === "server" ? "server" : "trusted" };
};

type ServerPasskeys = { readonly server: string; readonly passkeys: readonly { code: string; trusted: TrustedPasskey }[] | null };

/**
 * Each Server's own list of trusted passkeys, read from it directly through one entry. A Server that doesn't answer reads
 * `null`; with no entry at all, there is no Server to list.
 */
const trustedPasskeys = Effect.fn("Passkeys.trusted")(function* (organizationId: string) {
  const loaded = yield* loadOrganizationConnections(organizationId);
  if (loaded.kind !== "ready" || loaded.connections.length === 0) return [];
  const ployz = yield* Ployz;
  const frame = yield* firstRuntimeFrame(organizationId).pipe(Effect.orElseSucceed(() => null));
  const expect = loaded.sealingPublicKey === null ? null : { sealingPublicKey: loaded.sealingPublicKey, allowUnverified: true };
  const listed = (details: MachineDetails) =>
    (details.passkeys ?? []).map((trusted) => ({ code: codeOfPasskeyKey(trusted.passkey.public_key), trusted }));
  return yield* Effect.scoped(Effect.gen(function* () {
    const session = yield* ployz.connect({ connections: loaded.connections, timeoutMs: SERVER_REACH_TIMEOUT_MS, expect }, { closeWithin: clusterRead });
    const machines = frame?.machines.map(({ machine }) => ({ id: machine.id, name: machine.name })) ?? [];
    if (machines.length === 0) {
      const details = yield* session.inspect();
      return [{ server: details.machine?.name ?? details.id, passkeys: listed(details) }] satisfies ServerPasskeys[];
    }
    return yield* Effect.forEach(machines, ({ id, name }) => session.inspectMachine(id).pipe(
      Effect.map((details): ServerPasskeys => ({ server: name, passkeys: listed(details) })),
      Effect.orElseSucceed((): ServerPasskeys => ({ server: name, passkeys: null })),
    ), { concurrency: "unbounded" });
  })).pipe(clusterRead, Effect.orElseSucceed((): ServerPasskeys[] => []));
});
