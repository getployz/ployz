import { createHash } from "node:crypto";
import { Cause, Effect, Exit } from "effect";
import { afterAll, beforeAll, beforeEach, expect, it } from "vitest";
import {
  createFleetToken,
  listFleetTokens,
  loadFleetToken,
  revokeFleetToken,
} from "#/modules/machines/waiting/fleet-token.server";
import type { Database } from "#/server/database.server";
import { type PostgresTestHarness, startPostgresTestHarness } from "#/test/postgres";

const ada = { userId: "00000000-0000-4000-8000-000000000501" };
const bob = { userId: "00000000-0000-4000-8000-000000000502" };

let harness: PostgresTestHarness;

beforeAll(async () => {
  harness = await startPostgresTestHarness();
}, 60_000);

afterAll(async () => {
  await harness.stop();
});

beforeEach(async () => {
  await harness.pool.query(`
    truncate table fleet_token, "user", organization cascade;
    insert into organization (id, name, slug) values
      ('00000000-0000-4000-8000-000000000511', 'Ada', 'ada'),
      ('00000000-0000-4000-8000-000000000512', 'Bob', 'bob');
    insert into "user" (id, email, name) values
      ('${ada.userId}', 'ada@example.test', 'Ada'),
      ('${bob.userId}', 'bob@example.test', 'Bob');
    insert into member (id, organization_id, user_id, role, created_at) values
      (gen_random_uuid(), '00000000-0000-4000-8000-000000000511', '${ada.userId}', 'owner', now()),
      (gen_random_uuid(), '00000000-0000-4000-8000-000000000512', '${bob.userId}', 'owner', now());
  `);
});

const create = (name: string | null, generateName?: () => string) =>
  harness.runEffect(createFleetToken(ada, { organizationSlug: "ada", name, expiresIn: "1d", autoEnroll: null }, generateName));

const failure = async <A, E>(operation: Effect.Effect<A, E, Database>) => {
  const exit = await harness.runEffect(Effect.exit(operation));
  return Exit.isFailure(exit) ? Cause.squash(exit.cause) : null;
};

const revokedAt = async (id: string) =>
  (await harness.pool.query<{ revoked_at: Date | null }>("select revoked_at from fleet_token where id = $1", [id])).rows[0]?.revoked_at;

it("stores only the sha256 of the token, and loads it only while it is live", async () => {
  const created = await create("rack-1");
  expect(created.token).toMatch(/^pflt_[\w-]{43}$/);
  const { rows } = await harness.pool.query("select * from fleet_token");
  expect(rows.map((row) => row.token_hash)).toEqual([createHash("sha256").update(created.token).digest("hex")]);
  expect(JSON.stringify(rows)).not.toContain(created.token);

  expect(await harness.runEffect(loadFleetToken(created.token))).toMatchObject({ id: created.id, name: "rack-1" });
  expect(await harness.runEffect(loadFleetToken(`${created.token}x`))).toBeNull();

  await harness.pool.query("update fleet_token set expires_at = now() - interval '1 second' where id = $1", [created.id]);
  expect(await harness.runEffect(loadFleetToken(created.token))).toBeNull();
  const listed = await harness.runEffect(listFleetTokens(ada, { organizationSlug: "ada" }));
  expect(listed.map((token) => [token.name, token.expired])).toEqual([["rack-1", true]]);
});

it("retries a generated name that is taken, and refuses an explicit one", async () => {
  await create("amber-falcon");
  const names = ["amber-falcon", "amber-falcon", "bold-owl"];
  const created = await create(null, () => names.shift() ?? "unused");
  expect(created.name).toBe("bold-owl");
  expect(names).toEqual([]);

  expect(await failure(
    createFleetToken(ada, { organizationSlug: "ada", name: "amber-falcon", expiresIn: "7d", autoEnroll: null }),
  )).toMatchObject({ _tag: "Conflict", message: "A Fleet Token named amber-falcon already exists." });
  expect(await failure(
    createFleetToken(ada, { organizationSlug: "ada", name: null, expiresIn: "7d", autoEnroll: null }, () => "amber-falcon"),
  )).toMatchObject({ _tag: "Conflict", message: "Couldn't find a free name; give the token one." });
});

it("revokes once, treats a second revoke as a no-op, and frees the name", async () => {
  const created = await create("rack-1");
  expect(await harness.runEffect(revokeFleetToken(ada, { organizationSlug: "ada", id: created.id }))).toEqual({ waitingRemoved: 0 });
  const first = await revokedAt(created.id);
  expect(first).toBeInstanceOf(Date);
  expect(await harness.runEffect(loadFleetToken(created.token))).toBeNull();

  expect(await harness.runEffect(revokeFleetToken(ada, { organizationSlug: "ada", id: created.id }))).toEqual({ waitingRemoved: 0 });
  expect(await revokedAt(created.id)).toEqual(first);

  expect((await create("rack-1")).name).toBe("rack-1");
});

it("refuses to revoke another Organization's token", async () => {
  const created = await create("rack-1");
  for (const target of [{ id: created.id }, { name: "rack-1" }]) {
    expect(await failure(revokeFleetToken(bob, { organizationSlug: "bob", ...target })))
      .toMatchObject({ _tag: "NotFound", message: "No such Fleet Token." });
  }
  expect(await failure(revokeFleetToken(bob, { organizationSlug: "ada", id: created.id }))).toMatchObject({ _tag: "NotFound" });
  expect(await revokedAt(created.id)).toBeNull();
});
