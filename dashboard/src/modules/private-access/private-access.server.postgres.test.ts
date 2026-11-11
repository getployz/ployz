import type { PrivateAccessOn } from "@ployz/sdk";
import { Effect, Fiber } from "effect";
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
import { TAILSCALE } from "#/modules/private-access/private-access";
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
import { Database } from "#/server/database.server";
import { type PostgresTestHarness, startPostgresTestHarness } from "#/test/postgres";

const ORG = "00000000-0000-4000-8000-0000000c4a01";
const ADA = "00000000-0000-4000-8000-0000000c4a02";
const SLUG = "acme";
/** A key the browser sealed and a passkey approved. Cloud relays it as it came and keeps none of it. */
const SEALED = "c2VhbGVkLWluLXRoZS1icm93c2VyLW5ldmVyLXN0b3JlZC1ieS1DbG91ZC0wMTIzNDU2Nzg5";
const ON: PrivateAccessOn = {
  sealed_auth_key: SEALED,
  approval: { passkey: { body: "{\"signer\":\"passkey\"}", assertion: { credential_id: "AQID", authenticator_data: "AQID", client_data_json: "AQID", signature: "AQID" } } },
};
const SERVER = "a".repeat(32) as MachineId;

let harness: PostgresTestHarness;
const inngest = new Inngest({ id: "private-access-test" });
const sent = vi.spyOn(inngest, "send").mockResolvedValue({ ids: [] });

const handed: (PrivateAccessOn | null)[] = [];
let refuses = false;
let paused: Promise<void> | null = null;
let fails = false;
let unanswered = false;
let unreadable = false;
let disconnected = false;
const routers: ContainerObservation[] = [];
const placeRouter = (runtime: ContainerObservation["runtime"] = { state: "running", health: "none" }) => {
  routers.push({ ...runtimeWatchContainerFixture(SERVER, `router-${routers.length + 1}`), created_at_unix_nanos: Date.now() * 1e6, runtime });
};
const frame = () => runtimeWatchFrameFixture({
  machines: [runtimeWatchMachineObservationFixture({ machine: runtimeWatchMachineFixture(SERVER, "fra-1") })],
  services: [{ identity: TAILSCALE, service_id: "t".repeat(32) as ServiceId, containers: [...routers], hook_containers: [] }],
});
const session: ScopedRuntimeClientSession = {
  status: "connected",
  connected: asTestDouble<PloyzSession>()({
    watchFirstFrame: () => unreadable ? Effect.fail(new Error("no frame")) : Effect.sync(frame),
    setPrivateAccess: (on: PrivateAccessOn | null) => Effect.gen(function* () {
      handed.push(on);
      if (paused !== null) yield* Effect.promise(() => paused ?? Promise.resolve());
      if (fails) return yield* Effect.fail(new Error("connection reset"));
      if (on !== null && !refuses) placeRouter();
      const failure = { machine_id: SERVER, error: unanswered
        ? { code: "unavailable", message: "fra-1 didn't answer.", details: null, cause: [] }
        : { code: "unavailable", message: "Tailscale refused the auth key on fra-1. It may be revoked or expired.", details: { problem: "key_refused" }, cause: [] } };
      return {
        machines: [{ machine: SERVER, server: "fra-1", router: on !== null && !refuses }],
        failures: refuses ? [failure] : [],
      } satisfies AccessChanged;
    }),
  }),
};
const runtime = asTestDouble<OrganizationRuntime["Service"]>()({
  cancel: () => Effect.void,
  open: () => Effect.succeed<ScopedRuntimeClientSession>(disconnected ? { status: "unreachable", error: null } : session),
});

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

const connect = (approval: { id: string; digest: string }) =>
  approvePrivateAccess(dashboard, { id: approval.id, digest: approval.digest, on: ON });

const keyKept = async () => {
  const stored = await everyRow();
  const events = JSON.stringify(sent.mock.calls);
  return { stored: stored.includes(SEALED.slice(0, 24)), events: events.includes(SEALED.slice(0, 24)) };
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
  unreadable = false;
  disconnected = false;
  routers.length = 0;
  sent.mockClear();
  await harness.pool.query(`
    truncate table operation_outbox, operation_approvals, "user", organization cascade;
    insert into organization (id, name, slug) values ('${ORG}', 'Acme', '${SLUG}');
    insert into "user" (id, email, name) values ('${ADA}', 'ada@example.test', 'Ada');
    insert into member (id, organization_id, user_id, role, created_at) values (gen_random_uuid(), '${ORG}', '${ADA}', 'owner', now());
  `);
});

it("relays the key the browser sealed and approved to the Servers as it came, and stores it nowhere", async () => {
  const changed = await run(setPrivateAccess(ORG, ON));
  expect(changed).toEqual({ servers: [{ id: SERVER, name: "fra-1", router: true }], failures: [] });
  expect(handed).toEqual([ON]);
  expect(await keyKept()).toEqual({ stored: false, events: false });
});

it("asks an agent's turn-on as an Approval, which the approver decides with a sealed, approved key, and stores the key nowhere", async () => {
  const asked = await run(privateAccessUse.invoke(agent, { to: "on" }, { approvalId: null, key: null }));
  expect(asked).toMatchObject({ ok: false, refusal: { code: "approval_required" } });
  expect(handed).toEqual([]);
  const approval = await onlyApproval();
  expect(approval.status).toBe("pending");

  const generic = await run(decideApproval(dashboard, approval.id, { approve: { digest: approval.digest } }));
  expect(generic).toMatchObject({ ok: false, refusal: { code: "forbidden", message: "Approve it in Ployz Cloud, where you paste the Tailscale auth key." } });

  refuses = true;
  const refused = await run(connect(approval));
  expect(refused).toMatchObject({ approval: { status: "approved" }, sent: { failures: [{ problem: "key_refused" }] } });
  expect(await statusOf(approval.id)).toBe("approved");

  refuses = false;
  await expect(run(connect(approval))).rejects.toMatchObject({ _tag: "Conflict" });
  const retried = await run(setPrivateAccess(ORG, ON));
  expect(retried.failures).toEqual([]);
  expect(handed).toEqual([ON, ON]);
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

const deny = (id: string) => decideApproval(dashboard, id, { reject: { reason: "not now" } }, freshOperationDigest);

const holdConnect = () => {
  let release = () => {};
  paused = new Promise((resolve) => { release = resolve; });
  return () => release();
};

it("decides before the key goes out, so a deny or a second key during Connect finds the Approval approved", async () => {
  const approval = await askedOn();
  const release = holdConnect();
  const connecting = run(connect(approval));
  await vi.waitFor(() => expect(handed).toEqual([ON]));
  expect(await statusOf(approval.id)).toBe("approved");

  expect(await run(deny(approval.id))).toMatchObject({ ok: false, refusal: { code: "conflict", message: "This approval was already decided elsewhere (approved)." } });
  await expect(run(connect(approval))).rejects.toMatchObject({ _tag: "Conflict" });
  expect(handed).toEqual([ON]);

  release();
  expect(await connecting).toMatchObject({ approval: { status: "approved" }, sent: { failures: [] } });
  expect(await statusOf(approval.id)).toBe("approved");
});

it("asks an opposite turn-off during Connect as its own Approval, leaving the decided turn-on as it is", async () => {
  const approval = await askedOn();
  const release = holdConnect();
  const connecting = run(connect(approval));
  await vi.waitFor(() => expect(handed).toEqual([ON]));

  const off = await run(privateAccessUse.invoke(agent, { to: "off" }, { approvalId: null, key: null }));
  expect(off).toMatchObject({ ok: false, refusal: { code: "approval_required" } });
  const { rows } = await harness.pool.query<{ name: string; status: string }>("select review->'operation'->>'name' as name, status from operation_approvals order by created_at");
  expect(rows).toEqual([{ name: "on", status: "approved" }, { name: "off", status: "pending" }]);

  release();
  expect(await connecting).toMatchObject({ approval: { status: "approved" } });
});

it("supersedes a turn-on asked before an opposite turn-off, so a later Connect sends no key", async () => {
  const approval = await askedOn();
  await run(privateAccessUse.invoke(agent, { to: "off" }, { approvalId: null, key: null }));
  expect(await statusOf(approval.id)).toBe("superseded");
  await expect(run(connect(approval))).rejects.toMatchObject({ _tag: "Conflict" });
  expect(handed).toEqual([]);
});

it("tells the waiting agent the Servers are still connecting while Connect runs, then what they show", async () => {
  const approval = await askedOn();
  const release = holdConnect();
  const connecting = run(connect(approval));
  await vi.waitFor(() => expect(handed).toEqual([ON]));

  const waiting = await run(privateAccessUse.invoke(agent, { to: "on" }, { approvalId: approval.id, key: null }));
  expect(waiting).toMatchObject({ ok: true, output: { to: "on", routers: 0 } });
  expect(waiting.ok && waiting.message).toMatch(/^Servers are still connecting/u);

  release();
  await connecting;
  const shown = await run(privateAccessUse.invoke(agent, { to: "on" }, { approvalId: approval.id, key: null }));
  expect(shown).toMatchObject({ ok: false, refusal: { code: "approval_required" } });
  expect(handed).toEqual([ON]);
});

it("keeps the Approval approved when Cloud stops waiting for the Servers, whatever a later read of the Cluster shows", async () => {
  const approval = await askedOn();
  paused = new Promise(() => {});
  const timedOut = await run(Effect.gen(function* () {
    const connecting = yield* Effect.forkChild(connect(approval));
    yield* Effect.promise(() => vi.waitFor(() => expect(handed).toEqual([ON])));
    yield* TestClock.adjust("91 seconds");
    return yield* Fiber.join(connecting);
  }).pipe(Effect.provide(TestClock.layer())));
  expect(timedOut).toMatchObject({ approval: { status: "approved" }, sent: { problem: "Your servers didn't finish in time. Check again in a minute." } });

  unreadable = true;
  expect(await freshStatus(approval.id)).toBe("approved");
  expect(await run(deny(approval.id))).toMatchObject({ ok: false, refusal: { code: "conflict" } });
  unreadable = false;
  placeRouter({ state: "exited", code: 0 });
  expect(await freshStatus(approval.id)).toBe("approved");
});

it("refuses Connect before deciding when the Servers can't be reached, so the ask stays pending", async () => {
  const approval = await askedOn();
  disconnected = true;
  await expect(run(connect(approval))).rejects.toMatchObject({ _tag: "Validation" });
  expect(handed).toEqual([]);
  expect(await statusOf(approval.id)).toBe("pending");
});

it("names a refused key apart from a Server that didn't answer", async () => {
  refuses = true;
  const changed = await run(setPrivateAccess(ORG, ON));
  expect(changed.failures.map(({ problem }) => problem)).toEqual(["key_refused"]);
  unanswered = true;
  const unreached = await run(setPrivateAccess(ORG, ON));
  expect(unreached.failures.map(({ problem }) => problem)).toEqual(["unreachable"]);
});
