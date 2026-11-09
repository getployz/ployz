import { Effect, Redacted } from "effect";
import { Inngest } from "inngest";
import type { AccessChanged, MachineId } from "@ployz/sdk";
import { afterAll, beforeAll, beforeEach, expect, it, vi } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import type { Caller } from "#/modules/identity/actor";
import { FULL_GRANT } from "#/modules/identity/token-grant";
import { decideApproval } from "#/modules/approvals/approvals.server";
import { CloudStore } from "#/modules/config-store/store-sdk.server";
import { InngestClient } from "#/modules/inngest/client";
import { privateAccessUse } from "#/modules/private-access/private-access.commands.server";
import { approvePrivateAccess, setPrivateAccess } from "#/modules/private-access/private-access.server";
import { OrganizationRuntime, type ScopedRuntimeClientSession } from "#/modules/runtime/organization-runtime.server";
import type { PloyzSession } from "#/modules/runtime/ployz.server";
import type { Database } from "#/server/database.server";
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
const session: ScopedRuntimeClientSession = {
  status: "connected",
  connected: asTestDouble<PloyzSession>()({
    setPrivateAccess: (authKey: Redacted.Redacted<string> | null) => {
      handed.push(authKey === null ? null : Redacted.value(authKey));
      const failure = { machine_id: SERVER, error: { code: "unavailable", message: "Tailscale refused the auth key on fra-1. It may be revoked or expired.", details: { problem: "key_refused" }, cause: [] } };
      return Effect.succeed({
        machines: [{ machine: SERVER, server: "fra-1", router: authKey !== null && !refuses }],
        failures: refuses ? [failure] : [],
      } satisfies AccessChanged);
    },
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
  sent.mockClear();
  await harness.pool.query(`
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
