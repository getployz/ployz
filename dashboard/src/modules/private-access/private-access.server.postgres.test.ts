import { Effect, Fiber, Redacted } from "effect";
import { TestClock } from "effect/testing";
import { Inngest } from "inngest";
import type { AccessChanged, ContainerObservation, MachineId, ServiceId } from "@ployz/sdk";
import { afterAll, beforeAll, beforeEach, expect, it, vi } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import type { Caller } from "#/modules/identity/actor";
import { FULL_GRANT } from "#/modules/identity/token-grant";
import { decideApproval } from "#/modules/approvals/approvals.server";
import { CloudStore } from "#/modules/config-store/store-sdk.server";
import { InngestClient } from "#/modules/inngest/client";
import { freshApproval, freshOperationDigest } from "#/modules/machines/server-operations.server";
import { REFUSED_RESTARTS_MS, TAILSCALE } from "#/modules/private-access/private-access";
import { privateAccessUse } from "#/modules/private-access/private-access.commands.server";
import { approvePrivateAccess, setPrivateAccess } from "#/modules/private-access/private-access.server";
import { OrganizationRuntime, type ScopedRuntimeClientSession } from "#/modules/runtime/organization-runtime.server";
import type { PloyzSession } from "#/modules/runtime/ployz.server";
import {
  runtimeWatchContainerFixture,
  runtimeWatchFrameFixture,
  runtimeWatchMachineFixture,
  runtimeWatchMachineObservationFixture,
} from "#/modules/runtime/runtime-watch-frame.test-fixture";
import { operationApprovals } from "#/modules/approvals/tables";
import { Database, type DatabaseService } from "#/server/database.server";
import { type PostgresTestHarness, startPostgresTestHarness } from "#/test/postgres";

const ORG = "00000000-0000-4000-8000-0000000c4a01";
const ADA = "00000000-0000-4000-8000-0000000c4a02";
const SLUG = "acme";
const KEY = "tskey-auth-kC4TEST1CNTRL-c4PlaintextNeverStoredAnywhere0123456789";
const SERVER = "a".repeat(32) as MachineId;

let harness: PostgresTestHarness;
const inngest = new Inngest({ id: "private-access-test" });
const sent = vi.spyOn(inngest, "send").mockResolvedValue({ ids: [] });

const handed: (string | null)[] = [];
let refuses = false;
let paused: Promise<void> | null = null;
let fails = false;
let unanswered = false;
const routers: ContainerObservation[] = [];
const placeRouter = (runtime: ContainerObservation["runtime"] = { state: "running", health: "none" }) => {
  routers.push({ ...runtimeWatchContainerFixture(SERVER, `router-${routers.length + 1}`), created_at_unix_nanos: Date.now() * 1e6, runtime });
};
const ageRouters = (ms: number) => {
  for (const router of routers) router.created_at_unix_nanos -= ms * 1e6;
};
const frame = () => runtimeWatchFrameFixture({
  machines: [runtimeWatchMachineObservationFixture({ machine: runtimeWatchMachineFixture(SERVER, "fra-1") })],
  services: [{ identity: TAILSCALE, service_id: "t".repeat(32) as ServiceId, containers: [...routers], hook_containers: [] }],
});
const session: ScopedRuntimeClientSession = {
  status: "connected",
  connected: asTestDouble<PloyzSession>()({
    watchFirstFrame: () => Effect.sync(frame),
    setPrivateAccess: (authKey: Redacted.Redacted<string> | null) => Effect.gen(function* () {
      handed.push(authKey === null ? null : Redacted.value(authKey));
      if (paused !== null) yield* Effect.promise(() => paused ?? Promise.resolve());
      if (fails) return yield* Effect.fail(new Error("connection reset"));
      if (authKey !== null && !refuses) placeRouter();
      const failure = { machine_id: SERVER, error: unanswered
        ? { code: "unavailable", message: "fra-1 didn't answer.", details: null, cause: [] }
        : { code: "unavailable", message: "Tailscale refused the auth key on fra-1. It may be revoked or expired.", details: { problem: "key_refused" }, cause: [] } };
      return {
        machines: [{ machine: SERVER, server: "fra-1", router: authKey !== null && !refuses }],
        failures: refuses ? [failure] : [],
      } satisfies AccessChanged;
    }),
  }),
};
const runtime = asTestDouble<OrganizationRuntime["Service"]>()({ cancel: () => Effect.void, open: () => Effect.succeed(session) });

const store = asTestDouble<CloudStore["Service"]>()({});

const run = <A, E>(effect: Effect.Effect<A, E, Database | InngestClient | OrganizationRuntime | CloudStore>) =>
  harness.runEffect(effect.pipe(
    Effect.provideService(InngestClient, inngest),
    Effect.provideService(OrganizationRuntime, runtime),
    Effect.provideService(CloudStore, store),
  ));

const agent: Caller = {
  userId: ADA,
  organization: { id: ORG, slug: SLUG },
  credential: { kind: "token", id: "ada-agent", grant: FULL_GRANT },
  role: "owner",
  support: null,
  via: "agent",
};
const dashboard = { userId: ADA, organization: { id: ORG, slug: SLUG }, via: "dashboard" as const, permissions: ["approvals:answer" as const] };

async function everyRow() {
  const tables = await harness.pool.query<{ name: string }>("select quote_ident(table_name) as name from information_schema.tables where table_schema = 'public' and table_type = 'BASE TABLE'");
  const rows = await Promise.all(tables.rows.map(({ name }) => harness.pool.query<{ row: string }>(`select row_to_json(t)::text as row from ${name} t`)));
  return rows.flatMap(({ rows: found }) => found.map(({ row }) => row)).join("\n");
}

async function onlyApproval() {
  const { rows } = await harness.pool.query<{ id: string; digest: string; status: string }>("select id, digest, status from operation_approvals");
  const [approval] = rows;
  if (rows.length !== 1 || approval === undefined) throw new Error(`expected one approval, found ${rows.length}`);
  return approval;
}

const keyKept = async () => {
  const stored = await everyRow();
  const events = JSON.stringify(sent.mock.calls);
  return { stored: stored.includes(KEY) || stored.includes(KEY.slice(0, 24)), events: events.includes(KEY.slice(0, 24)) };
};

beforeAll(async () => {
  harness = await startPostgresTestHarness();
}, 60_000);

afterAll(async () => {
  await harness.stop();
});

beforeEach(async () => {
  handed.length = 0;
  refuses = false;
  paused = null;
  fails = false;
  unanswered = false;
  routers.length = 0;
  sent.mockClear();
  await harness.pool.query(`
    drop trigger if exists approving on operation_approvals;
    truncate table operation_outbox, operation_approvals, "user", organization cascade;
    insert into organization (id, name, slug) values ('${ORG}', 'Acme', '${SLUG}');
    insert into "user" (id, email, name) values ('${ADA}', 'ada@example.test', 'Ada');
    insert into member (id, organization_id, user_id, role, created_at) values (gen_random_uuid(), '${ORG}', '${ADA}', 'owner', now());
  `);
});

it("hands the key from the dashboard to the Servers and stores it nowhere", async () => {
  const changed = await run(setPrivateAccess(ORG, Redacted.make(KEY)));
  expect(changed).toEqual({ servers: [{ id: SERVER, name: "fra-1", router: true }], failures: [] });
  expect(handed).toEqual([KEY]);
  expect(await keyKept()).toEqual({ stored: false, events: false });
});

it("asks an agent's turn-on as an Approval, which the approver finishes with the key, and stores the key nowhere", async () => {
  const asked = await run(privateAccessUse.invoke(agent, { to: "on" }, { approvalId: null, key: null }));
  expect(asked).toMatchObject({ ok: false, refusal: { code: "approval_required" } });
  expect(handed).toEqual([]);
  const approval = await onlyApproval();
  expect(approval.status).toBe("pending");

  const generic = await run(decideApproval(dashboard, approval.id, { approve: { digest: approval.digest } }));
  expect(generic).toMatchObject({ ok: false, refusal: { code: "forbidden", message: "Approve it in Ployz Cloud, where you paste the Tailscale auth key." } });

  refuses = true;
  const refused = await run(approvePrivateAccess(dashboard, { id: approval.id, digest: approval.digest, authKey: Redacted.make(KEY) }));
  expect(refused.decided).toBeNull();
  expect(refused.changed.failures.map(({ message }) => message)).toEqual(["Tailscale refused the auth key on fra-1. It may be revoked or expired."]);
  expect((await harness.pool.query("select status from operation_approvals")).rows).toEqual([{ status: "pending" }]);

  refuses = false;
  const approved = await run(approvePrivateAccess(dashboard, { id: approval.id, digest: approval.digest, authKey: Redacted.make(KEY) }));
  expect(approved.decided?.status).toBe("approved");
  expect(handed).toEqual([KEY, KEY]);
  expect(await keyKept()).toEqual({ stored: false, events: false });
});

it("leaves everything as it was when the agent's ask is denied", async () => {
  await run(privateAccessUse.invoke(agent, { to: "on" }, { approvalId: null, key: null }));
  const approval = await onlyApproval();
  await run(decideApproval(dashboard, approval.id, { reject: { reason: "not now" } }));
  const again = await run(privateAccessUse.invoke(agent, { to: "on" }, { approvalId: approval.id, key: null }));
  expect(again).toMatchObject({ ok: false });
  expect(handed).toEqual([]);
  expect((await harness.pool.query("select status from operation_approvals where id = $1", [approval.id])).rows).toEqual([{ status: "denied" }]);
});

const statusOf = async (id: string) =>
  (await harness.pool.query<{ status: string }>("select status from operation_approvals where id = $1", [id])).rows[0]?.status;

const freshStatus = (id: string) => run(Effect.map(freshApproval(ORG, id), ({ status }) => status));

const askedOn = async () => {
  await run(privateAccessUse.invoke(agent, { to: "on" }, { approvalId: null, key: null }));
  return onlyApproval();
};

const connect = (approval: { id: string; digest: string }) =>
  approvePrivateAccess(dashboard, { id: approval.id, digest: approval.digest, authKey: Redacted.make(KEY) });

type ApprovalWrite = Partial<typeof operationApprovals.$inferInsert>;

const claims = (values: ApprovalWrite) => (values.claim ?? null) !== null || values.usedAt instanceof Date;

/** Make a write that claims a row commit, then answer only once `gate` settles, so a test can abort between the two. */
const answerClaimLate = (drizzle: DatabaseService["drizzle"], gate: Promise<void>, written: () => void) => {
  const update = drizzle.update.bind(drizzle);
  drizzle.update = (table) => {
    const query = update(table);
    const set = query.set.bind(query);
    query.set = (values) => {
      const next = set(values);
      if (!claims(asTestDouble<ApprovalWrite>()(values))) return next;
      const answered = Effect.flatMap(asTestDouble<Effect.Effect<unknown>>()(next), (rows) =>
        Effect.as(Effect.promise(() => { written(); return gate; }), rows));
      return Object.assign(next, { [Symbol.iterator]: () => answered[Symbol.iterator]() });
    };
    return query;
  };
  return () => { drizzle.update = update; };
};

const deny = (id: string) => decideApproval(dashboard, id, { reject: { reason: "not now" } }, freshOperationDigest);

const blockedOn = async (statement: string) =>
  ((await harness.pool.query("select 1 from pg_stat_activity where wait_event_type = 'Lock' and query ilike $1", [`${statement}%`])).rowCount ?? 0) > 0;

it("holds the Approval while the Servers take the key, so a deny or a second key can't land until they answer", async () => {
  await run(privateAccessUse.invoke(agent, { to: "on" }, { approvalId: null, key: null }));
  const approval = await onlyApproval();
  let release = () => {};
  paused = new Promise((resolve) => { release = resolve; });
  const connecting = run(approvePrivateAccess(dashboard, { id: approval.id, digest: approval.digest, authKey: Redacted.make(KEY) }));
  await vi.waitFor(() => expect(handed).toEqual([KEY]));

  const denied = await run(decideApproval(dashboard, approval.id, { reject: { reason: "not now" } }));
  expect(denied).toMatchObject({ ok: false, refusal: { code: "conflict" } });
  await expect(run(approvePrivateAccess(dashboard, { id: approval.id, digest: approval.digest, authKey: Redacted.make(KEY) }))).rejects.toThrow();
  expect(handed).toEqual([KEY]);
  expect(await statusOf(approval.id)).toBe("pending");

  release();
  const connected = await connecting;
  expect(connected.decided?.status).toBe("approved");
  expect(await statusOf(approval.id)).toBe("approved");
  expect(await keyKept()).toEqual({ stored: false, events: false });
});

it("lets go of the Approval when the Servers fail, so a deny then lands", async () => {
  await run(privateAccessUse.invoke(agent, { to: "on" }, { approvalId: null, key: null }));
  const approval = await onlyApproval();
  fails = true;
  await expect(run(approvePrivateAccess(dashboard, { id: approval.id, digest: approval.digest, authKey: Redacted.make(KEY) }))).rejects.toThrow();
  expect(await statusOf(approval.id)).toBe("pending");

  const denied = await run(decideApproval(dashboard, approval.id, { reject: { reason: "not now" } }));
  expect(denied).toMatchObject({ ok: true, approval: { status: "denied" } });
});

it("lets a deny land once a claim whose Servers placed no router has outlived the work's bound", async () => {
  const approval = await askedOn();
  await harness.pool.query(`update operation_approvals set claim = jsonb_build_object('token', gen_random_uuid(), 'by', null, 'at', now() - interval '1 hour', 'before', '[]'::jsonb) where id = $1`, [approval.id]);
  const denied = await run(deny(approval.id));
  expect(denied).toMatchObject({ ok: true, approval: { status: "denied" } });
});

it("lets go when the request is aborted as the claim is written, so no key goes out and a deny lands", async () => {
  const approval = await askedOn();
  let answer = () => {};
  const gate = new Promise<void>((resolve) => { answer = resolve; });
  let written = () => {};
  const claimed = new Promise<void>((resolve) => { written = resolve; });
  await run(Effect.gen(function* () {
    const restore = answerClaimLate((yield* Database).drizzle, gate, written);
    const connecting = yield* Effect.forkChild(connect(approval));
    yield* Effect.promise(() => claimed);
    const aborting = yield* Effect.forkChild(Fiber.interrupt(connecting));
    yield* Fiber.await(aborting).pipe(Effect.timeoutOption("200 millis"));
    answer();
    yield* Fiber.join(aborting);
    restore();
  }));
  expect(handed).toEqual([]);
  expect(await run(deny(approval.id))).toMatchObject({ ok: true, approval: { status: "denied" } });
});

it("keeps the Approval claimed when the request is aborted mid-Connect, and approves it once the Servers answer", async () => {
  const approval = await askedOn();
  let release = () => {};
  paused = new Promise((resolve) => { release = resolve; });
  await run(Effect.gen(function* () {
    const connecting = yield* Effect.forkChild(connect(approval));
    yield* Effect.promise(() => vi.waitFor(() => expect(handed).toEqual([KEY])));
    const aborting = yield* Effect.forkChild(Fiber.interrupt(connecting));
    yield* Fiber.await(aborting).pipe(Effect.timeoutOption("200 millis"));
    expect(yield* deny(approval.id)).toMatchObject({ ok: false, refusal: { code: "conflict" } });
    expect(yield* Effect.flip(connect(approval))).toMatchObject({ _tag: "Conflict" });
    release();
    yield* Fiber.join(aborting);
  }));
  expect(handed).toEqual([KEY]);
  expect(await statusOf(approval.id)).toBe("approved");
});

it("keeps the Approval claimed past Cloud's wait, refusing a deny, until the Servers' new router has run past the refusal window", async () => {
  const approval = await askedOn();
  paused = new Promise(() => {});
  await run(Effect.gen(function* () {
    const connecting = yield* Effect.forkChild(connect(approval));
    yield* Effect.promise(() => vi.waitFor(() => expect(handed).toEqual([KEY])));
    yield* TestClock.adjust("91 seconds");
    expect(yield* Effect.flip(Fiber.join(connecting))).toMatchObject({ _tag: "Validation", message: "Your servers didn't finish in time. Check again in a minute." });
  }).pipe(Effect.provide(TestClock.layer())));
  expect(await run(deny(approval.id))).toMatchObject({ ok: false, refusal: { code: "conflict" } });

  placeRouter();
  expect(await freshStatus(approval.id)).toBe("pending");
  expect(await run(deny(approval.id))).toMatchObject({ ok: false, refusal: { code: "conflict" } });

  ageRouters(REFUSED_RESTARTS_MS);
  expect(await freshStatus(approval.id)).toBe("approved");
});

it("lets go once the Servers' new router stays stopped after Cloud stopped waiting, so a corrected key can follow", async () => {
  const approval = await askedOn();
  paused = new Promise(() => {});
  await run(Effect.gen(function* () {
    const connecting = yield* Effect.forkChild(connect(approval));
    yield* Effect.promise(() => vi.waitFor(() => expect(handed).toEqual([KEY])));
    yield* TestClock.adjust("91 seconds");
    yield* Effect.flip(Fiber.join(connecting));
  }).pipe(Effect.provide(TestClock.layer())));
  placeRouter({ state: "exited", code: 0 });
  ageRouters(REFUSED_RESTARTS_MS);
  expect(await freshStatus(approval.id)).toBe("pending");
  paused = null;
  expect((await run(connect(approval))).decided?.status).toBe("approved");
});

it("approves once the Servers took the key, even when the request is aborted during the final write", async () => {
  const approval = await askedOn();
  await harness.pool.query(`
    create or replace function pause_approving() returns trigger language plpgsql as $$ begin perform pg_advisory_xact_lock(4029); return new; end $$;
    create trigger approving before update on operation_approvals for each row when (new.status = 'approved') execute function pause_approving();
  `);
  const locker = await harness.pool.connect();
  try {
    await locker.query("select pg_advisory_lock(4029)");
    await run(Effect.gen(function* () {
      const connecting = yield* Effect.forkChild(connect(approval));
      yield* Effect.promise(() => vi.waitFor(async () => expect(await blockedOn('update "operation_approvals" set "status"')).toBe(true)));
      const aborting = yield* Effect.forkChild(Fiber.interrupt(connecting));
      yield* Effect.promise(() => locker.query("select pg_advisory_unlock(4029)"));
      yield* Fiber.join(aborting);
    }));
  } finally {
    await locker.query("select pg_advisory_unlock_all()");
    locker.release();
  }
  expect(await statusOf(approval.id)).toBe("approved");
});

it("keeps the Approval claimed when the final write fails, and approves it from the Servers' routers once they read", async () => {
  const approval = await askedOn();
  await harness.pool.query(`
    create or replace function refuse_approving() returns trigger language plpgsql as $$ begin raise exception 'the database refused the write'; end $$;
    create trigger approving before update on operation_approvals for each row when (new.status = 'approved') execute function refuse_approving();
  `);
  await expect(run(connect(approval))).rejects.toThrow();
  expect(await statusOf(approval.id)).toBe("pending");
  expect(await run(deny(approval.id))).toMatchObject({ ok: false, refusal: { code: "conflict" } });

  await harness.pool.query("drop trigger approving on operation_approvals");
  expect(await freshStatus(approval.id)).toBe("pending");
  ageRouters(REFUSED_RESTARTS_MS);
  expect(await freshStatus(approval.id)).toBe("approved");
});

it("names a refused key apart from a Server that didn't answer", async () => {
  refuses = true;
  const changed = await run(setPrivateAccess(ORG, Redacted.make(KEY)));
  expect(changed.failures.map(({ problem }) => problem)).toEqual(["key_refused"]);
  unanswered = true;
  const unreached = await run(setPrivateAccess(ORG, Redacted.make(KEY)));
  expect(unreached.failures.map(({ problem }) => problem)).toEqual(["unreachable"]);
});
