import { sql } from "drizzle-orm";
import type { Client } from "@ployz/sdk";
import { Effect, Exit, Layer, ManagedRuntime, Scope } from "effect";
import { expect, it } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { startGithubPostgresTestHarness } from "#/modules/github/github-ingestion.postgres-test-harness";
import { hashEnrollmentToken } from "#/modules/machines/enrollment.server";
import { OrganizationRuntime, OrganizationRuntimeLive } from "#/modules/runtime/organization-runtime.server";
import { makePloyzLayer } from "#/modules/runtime/ployz.server";
import { Database } from "#/server/database.server";
import { makeSecretEncryption, SecretEncryption } from "#/utils/encrypted-secret.server";

it("cancels both Cloud workers only after commit and preserves replacement pairings", async () => {
  const harness = await startGithubPostgresTestHarness();
  const organizationId = "00000000-0000-4000-8000-000000000881";
  const barrierOrganization = "00000000-0000-4000-8000-000000000882";
  const machineId = "00000000000000000000000000000001";
  const encryption = makeSecretEncryption("runtime-cancellation-test-encryption");
  const scopes: Scope.Closeable[] = [];
  const workers: ReturnType<typeof worker>[] = [];
  async function pair(id: string, secret: string) {
    await harness.pool.query("insert into organization (id, name, slug) values ($1,$2,$2) on conflict do nothing", [id, id]);
    await harness.pool.query(`
      insert into organization_pairing (organization_id, encrypted_pairing_secret, founder_claim_machine_id, founder_machine_id)
      values ($1,$2,$3,$3) on conflict (organization_id) do update set
      encrypted_pairing_secret = excluded.encrypted_pairing_secret, removal_started_at = null, removal_endpoints = null
    `, [id, encryption.encrypt(secret), machineId]);
    await harness.pool.query(`
      insert into organization_machine (organization_id, machine_id, cluster_key, encrypted_tailcat)
      values ($1,$2,$3,$4) on conflict (organization_id,machine_id) do update set cluster_key=excluded.cluster_key
    `, [id, machineId, hashEnrollmentToken(secret), encryption.encrypt("tailcat://test")]);
  }
  function worker() {
    const sessions: Array<{ closed: boolean; finished: Promise<void>; close: () => void }> = [];
    const runtime = ManagedRuntime.make(OrganizationRuntimeLive.pipe(Layer.provide(Layer.mergeAll(
      Layer.succeed(Database, harness.database),
      Layer.succeed(SecretEncryption, encryption),
      makePloyzLayer({ connect: async () => {
        let close = () => {};
        const finished = new Promise<void>((resolve) => { close = resolve; });
        const session = { closed: false, finished, close };
        sessions.push(session);
        return asTestDouble<Client>()({ close: async () => { session.closed = true; session.close(); } });
      } }),
    ))));
    return {
      runtime,
      sessions,
      async open(id: string) {
        const scope = await Effect.runPromise(Scope.make());
        scopes.push(scope);
        return runtime.runPromise(Effect.gen(function* () {
          return yield* (yield* OrganizationRuntime).open(id);
        }).pipe(Effect.provideService(Scope.Scope, scope)));
      },
    };
  }
  async function notify(id: string, secret: string) {
    await harness.pool.query("select pg_notify('ployz_pairing_removed', $1)", [
      JSON.stringify({ organizationId: id, generation: hashEnrollmentToken(secret) }),
    ]);
  }
  try {
    await pair(organizationId, "old");
    await pair(barrierOrganization, "barrier");
    workers.push(worker(), worker());
    for (const current of workers) {
      expect((await current.open(organizationId)).status).toBe("connected");
      expect((await current.open(barrierOrganization)).status).toBe("connected");
    }
    const transaction = await harness.pool.connect();
    try {
      await transaction.query("begin");
      await transaction.query("update organization_pairing set removal_started_at=now(), removal_endpoints='[]' where organization_id=$1", [organizationId]);
      await transaction.query("select pg_notify('ployz_pairing_removed', $1)", [JSON.stringify({ organizationId, generation: hashEnrollmentToken("old") })]);
      await transaction.query("rollback");
    } finally {
      transaction.release();
    }
    // A subsequent committed notification is a delivery barrier on each worker's listener.
    await notify(barrierOrganization, "barrier");
    await Promise.all(workers.map((current) => current.sessions[1]?.finished));
    for (const current of workers) expect(current.sessions[0]?.closed).toBe(false);

    await harness.runTransaction((database) => Effect.gen(function* () {
      yield* database.execute(sql`update organization_pairing set removal_started_at=now(), removal_endpoints='[]' where organization_id=${organizationId}`);
      yield* database.execute(sql`select pg_notify('ployz_pairing_removed', ${JSON.stringify({ organizationId, generation: hashEnrollmentToken("old") })})`);
    }));
    await Promise.all(workers.map((current) => current.sessions[0]?.finished));
    for (const current of workers) {
      expect(current.sessions[0]?.closed).toBe(true);
      expect(await current.open(organizationId)).toEqual({ status: "no_connection" });
      expect(current.sessions).toHaveLength(2);
    }
    const restarted = worker();
    workers.push(restarted);
    expect(await restarted.open(organizationId)).toEqual({ status: "no_connection" });
    expect(restarted.sessions).toHaveLength(0);

    await pair(organizationId, "replacement");
    const replacements = [];
    const barriers = [];
    for (const current of workers) {
      expect((await current.open(organizationId)).status).toBe("connected");
      replacements.push(current.sessions.at(-1));
      expect((await current.open(barrierOrganization)).status).toBe("connected");
      barriers.push(current.sessions.at(-1));
    }
    await notify(organizationId, "old");
    await notify(barrierOrganization, "barrier");
    await Promise.all(barriers.map((session) => session?.finished));
    expect(replacements.every((session) => session?.closed === false)).toBe(true);
  } finally {
    await Promise.all(scopes.map((scope) => Effect.runPromise(Scope.close(scope, Exit.void))));
    await Promise.all(workers.map((current) => current.runtime.dispose()));
    await harness.stop();
  }
}, 60_000);
