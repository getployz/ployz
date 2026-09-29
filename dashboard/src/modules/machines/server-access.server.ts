import "@tanstack/react-start/server-only";
import type { Connection, MachineId as SdkMachineId } from "@ployz/sdk";
import { and, eq, isNotNull, isNull, not, sql, type SQL } from "drizzle-orm";
import { Effect } from "effect";
import type { MachineId } from "#/db/tables";
import type { Caller } from "#/modules/identity/actor";
import { member, organizationToken, session } from "#/modules/identity/tables";
import { decryptPairingSecret, loadOrganizationConnections } from "#/modules/machines/connections.server";
import { organizationMachine, serverAccess } from "#/modules/machines/tables";
import { Ployz } from "#/modules/runtime/ployz.server";
import { Database } from "#/server/database.server";
import { SecretEncryption } from "#/utils/encrypted-secret.server";

/** One dial or Clear per Server; a Server that doesn't answer in time is unreachable or unconfirmed. */
const SERVER_TIMEOUT_MS = 10_000;
const SERVER_CONCURRENCY = 4;

/** A credential's Management Client label on every Server: `cli-` and the first 28 hex digits of its id. */
export function serverAccessLabel(credentialId: string) {
  return `cli-${credentialId.replaceAll("-", "").slice(0, 28)}`;
}

/** Set `label` on one Server through Cloud's own `cloud` slot. */
const setHolder = (connection: Connection, label: string) =>
  Effect.scoped(Effect.gen(function* () {
    const ployz = yield* Ployz;
    const client = yield* ployz.connect({ connections: [connection], timeoutMs: SERVER_TIMEOUT_MS });
    return yield* client.setManagementClient(label);
  })).pipe(Effect.timeout(SERVER_TIMEOUT_MS * 2));

/**
 * The Caller's own capability on every Server of its Organization, provisioning a missing `cli-<id>` holder on
 * first use, including on Servers enrolled after login. A Server that can't be reached now is listed as unreachable.
 */
export const provideServerAccess = Effect.fn("ServerAccess.provide")(function* (caller: Caller) {
  const loaded = yield* loadOrganizationConnections(caller.organization.id);
  if (loaded.kind === "missing") return { connections: [], unreachable: [] };
  const { drizzle } = yield* Database;
  const encryption = yield* SecretEncryption;
  const held = new Map((yield* drizzle.select({
    machineId: serverAccess.machineId,
    encryptedCapability: serverAccess.encryptedCapability,
  }).from(serverAccess).where(and(
    eq(serverAccess.organizationId, caller.organization.id),
    eq(serverAccess.credentialId, caller.credential.id),
    isNull(serverAccess.revokedAt),
  ))).map((row) => [row.machineId, row.encryptedCapability]));
  const label = serverAccessLabel(caller.credential.id);
  const results = yield* Effect.forEach(loaded.connections, (cloud) => Effect.gen(function* () {
    const machineId = cloud.machine_id;
    if (machineId === undefined) return yield* Effect.die("an Organization connection names its Machine");
    const stored = held.get(machineId);
    if (stored != null) return { machine_id: machineId, management: yield* decryptPairingSecret(stored) };
    const management = yield* setHolder(cloud, label).pipe(Effect.option);
    if (management._tag === "None") return { machine_id: machineId, management: null };
    // ponytail: two first uses racing both Set; the later key wins and the earlier command may need a rerun.
    // A revocation racing this insert is caught by the next retireServerAccess sweep.
    const encryptedCapability = encryption.encrypt(management.value);
    yield* drizzle.insert(serverAccess).values({
      organizationId: caller.organization.id,
      machineId,
      credentialId: caller.credential.id,
      credentialKind: caller.credential.kind,
      userId: caller.userId,
      encryptedCapability,
    }).onConflictDoUpdate({
      target: [serverAccess.organizationId, serverAccess.credentialId, serverAccess.machineId],
      set: { encryptedCapability, revokedAt: null, updatedAt: new Date() },
    });
    return { machine_id: machineId, management: management.value };
  }), { concurrency: SERVER_CONCURRENCY });
  return {
    connections: results.flatMap(({ machine_id, management }) => management === null ? [] : [{ machine_id, management }]),
    unreachable: results.flatMap(({ machine_id, management }) => management === null ? [machine_id] : []),
  };
});

/** A row's credential still authorizes it: unexpired, and its user still a member of the row's Organization. */
const stillAuthorized = sql`(exists (select 1 from ${member}
    where ${member.userId} = ${serverAccess.userId} and ${member.organizationId} = ${serverAccess.organizationId})
  and ((${serverAccess.credentialKind} = 'session' and exists (select 1 from ${session}
      where ${session.id} = ${serverAccess.credentialId} and ${session.expiresAt} > now()))
    or (${serverAccess.credentialKind} = 'token' and exists (select 1 from ${organizationToken}
      where ${organizationToken.id} = ${serverAccess.credentialId}
        and ${organizationToken.organizationId} = ${serverAccess.organizationId}
        and ${organizationToken.expiresAt} > now()))))`;

/**
 * Revoke every holder in `scope` whose credential no longer authorizes it (logout, deletion, expiry, membership
 * removal): Cloud forgets its capability at once, then tries one bounded Clear per Server. A Clear that doesn't
 * confirm leaves the revocation pending for the next retry.
 */
export const retireServerAccess = Effect.fn("ServerAccess.retire")(function* (scope?: SQL) {
  const { drizzle } = yield* Database;
  yield* drizzle.update(serverAccess)
    .set({ revokedAt: new Date(), encryptedCapability: null, updatedAt: new Date() })
    .where(and(isNull(serverAccess.revokedAt), not(stillAuthorized), scope));
  const pending = yield* drizzle.select({
    organizationId: serverAccess.organizationId,
    machineId: serverAccess.machineId,
    credentialId: serverAccess.credentialId,
    cloud: organizationMachine.encryptedCapability,
  }).from(serverAccess)
    .innerJoin(organizationMachine, and(
      eq(organizationMachine.organizationId, serverAccess.organizationId),
      eq(organizationMachine.machineId, serverAccess.machineId),
    ))
    .where(and(isNotNull(serverAccess.revokedAt), scope));
  const outcomes = yield* Effect.forEach(pending, (row) => Effect.gen(function* () {
    const management = yield* decryptPairingSecret(row.cloud);
    yield* Effect.scoped(Effect.gen(function* () {
      const ployz = yield* Ployz;
      // SAFETY: the organization_machine constraint enforces the SDK Machine ID representation.
      const machineId = row.machineId as SdkMachineId;
      const client = yield* ployz.connect({ connections: [{ machine_id: machineId, management }], timeoutMs: SERVER_TIMEOUT_MS });
      // Clearing an absent or already-cleared slot succeeds, so a retry after a lost reply confirms.
      yield* client.clearManagementClient(serverAccessLabel(row.credentialId));
    })).pipe(Effect.timeout(SERVER_TIMEOUT_MS * 2));
    yield* drizzle.delete(serverAccess).where(and(
      eq(serverAccess.organizationId, row.organizationId),
      eq(serverAccess.credentialId, row.credentialId),
      eq(serverAccess.machineId, row.machineId),
      isNotNull(serverAccess.revokedAt),
    ));
    return { machineId: row.machineId, confirmed: true };
  }).pipe(Effect.orElseSucceed(() => ({ machineId: row.machineId, confirmed: false }))), { concurrency: SERVER_CONCURRENCY });
  return {
    confirmed: outcomes.flatMap((outcome) => outcome.confirmed ? [outcome.machineId] : []),
    unconfirmed: outcomes.flatMap((outcome) => outcome.confirmed ? [] : [outcome.machineId]),
  };
});

/** Retire one credential's holders, on every Server of every Organization it reached. */
export const retireCredentialServerAccess = (credentialId: string) =>
  retireServerAccess(eq(serverAccess.credentialId, credentialId));

/** Revoked credentials of the Organization whose Servers haven't confirmed the Clear yet. */
export const pendingServerRevocations = Effect.fn("ServerAccess.pending")(function* (organizationId: string) {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.select({
    id: serverAccess.credentialId,
    kind: serverAccess.credentialKind,
    machineId: serverAccess.machineId,
  }).from(serverAccess)
    .where(and(eq(serverAccess.organizationId, organizationId), isNotNull(serverAccess.revokedAt)))
    .orderBy(serverAccess.credentialId, serverAccess.machineId);
  const byCredential = new Map<string, { id: string; kind: "device" | "token"; unconfirmed: MachineId[] }>();
  for (const row of rows) {
    const entry = byCredential.get(row.id)
      ?? { id: row.id, kind: row.kind === "session" ? "device" as const : "token" as const, unconfirmed: [] };
    entry.unconfirmed.push(row.machineId);
    byCredential.set(row.id, entry);
  }
  return [...byCredential.values()];
});
