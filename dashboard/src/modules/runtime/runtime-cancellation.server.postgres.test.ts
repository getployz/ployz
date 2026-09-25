import { setTimeout as sleep } from "node:timers/promises";
import type { Client } from "@ployz/sdk";
import { Duration, Effect, Exit, Layer, ManagedRuntime, Scope } from "effect";
import { expect, it } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { startPostgresTestHarness } from "#/test/postgres";
import { hashEnrollmentToken } from "#/modules/machines/enrollment.server";
import { disableOrganizationPairing } from "#/modules/machines/pairing-removal.server";
import {
  OrganizationRuntime,
  OrganizationRuntimeLive,
  PAIRING_CHANGE_POLL,
} from "#/modules/runtime/organization-runtime.server";
import { makePloyzLayer } from "#/modules/runtime/ployz.server";
import { Database } from "#/server/database.server";
import { makeSecretEncryption, SecretEncryption } from "#/utils/encrypted-secret.server";

// Two polls: every open session has read the log past the last commit.
const settle = () => sleep(2 * Duration.toMillis(PAIRING_CHANGE_POLL) + 250);

it("closes every Cloud worker's session through the change log only after commit and preserves replacement pairings", async () => {
  const harness = await startPostgresTestHarness();
  const organizationId = "00000000-0000-4000-8000-000000000881";
  const machineId = "00000000000000000000000000000001";
  const encryption = makeSecretEncryption("runtime-cancellation-test-encryption");
  const scopes: Scope.Closeable[] = [];
  const workers: ReturnType<typeof worker>[] = [];
  async function pair(secret: string) {
    await harness.pool.query("insert into organization (id, name, slug) values ($1,$2,$2) on conflict do nothing", [organizationId, organizationId]);
    await harness.pool.query(`
      insert into organization_pairing (organization_id, encrypted_pairing_secret, founder_claim_machine_id, founder_machine_id)
      values ($1,$2,$3,$3) on conflict (organization_id) do update set
      encrypted_pairing_secret = excluded.encrypted_pairing_secret, removal_started_at = null, removal_endpoints = null
    `, [organizationId, encryption.encrypt(secret), machineId]);
    await harness.pool.query(`
      insert into organization_machine (organization_id, machine_id, cluster_key, encrypted_capability)
      values ($1,$2,$3,$4) on conflict (organization_id,machine_id) do update set cluster_key=excluded.cluster_key
    `, [organizationId, machineId, hashEnrollmentToken(secret), encryption.encrypt("ployz1:test")]);
  }
  function worker() {
    const sessions: Array<{ closed: boolean }> = [];
    const runtime = ManagedRuntime.make(OrganizationRuntimeLive.pipe(Layer.provideMerge(Layer.mergeAll(
      Layer.succeed(Database, harness.database),
      Layer.succeed(SecretEncryption, encryption),
      makePloyzLayer({ connect: async () => {
        const session = { closed: false };
        sessions.push(session);
        return asTestDouble<Client>()({ close: async () => { session.closed = true; } });
      } }),
    ))));
    return {
      runtime,
      sessions,
      async open() {
        const scope = await Effect.runPromise(Scope.make());
        scopes.push(scope);
        return runtime.runPromise(Effect.gen(function* () {
          return yield* (yield* OrganizationRuntime).open(organizationId);
        }).pipe(Effect.provideService(Scope.Scope, scope)));
      },
    };
  }
  try {
    await pair("old");
    workers.push(worker(), worker());
    for (const current of workers) expect((await current.open()).status).toBe("connected");

    const transaction = await harness.pool.connect();
    try {
      await transaction.query("begin");
      await transaction.query("update organization_pairing set removal_started_at=now(), removal_endpoints='[]' where organization_id=$1", [organizationId]);
      await transaction.query("rollback");
    } finally {
      transaction.release();
    }
    await settle();
    for (const current of workers) expect(current.sessions[0]?.closed).toBe(false);

    // The first worker removes the pairing; the second learns of it only through the log.
    const [remover, other] = workers;
    await remover?.runtime.runPromise(disableOrganizationPairing(organizationId));
    expect(remover?.sessions[0]?.closed).toBe(true);
    await settle();
    expect(other?.sessions[0]?.closed).toBe(true);
    for (const current of workers) {
      expect(await current.open()).toEqual({ status: "no_connection" });
      expect(current.sessions).toHaveLength(1);
    }

    await pair("replacement");
    for (const current of workers) expect((await current.open()).status).toBe("connected");
    // A later pairing write that keeps the generation re-checks and keeps the session.
    await harness.pool.query("update organization_pairing set founder_machine_id = founder_machine_id where organization_id=$1", [organizationId]);
    await settle();
    for (const current of workers) expect(current.sessions.at(-1)?.closed).toBe(false);
  } finally {
    await Promise.all(scopes.map((scope) => Effect.runPromise(Scope.close(scope, Exit.void))));
    await Promise.all(workers.map((current) => current.runtime.dispose()));
    await harness.stop();
  }
}, 60_000);
