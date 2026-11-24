import type { PrivateAccessOn } from "@ployz/sdk";
import { Effect } from "effect";
import { Inngest } from "inngest";
import type { AccessChanged, ContainerObservation, MachineId, ServiceId } from "@ployz/sdk";
import { afterAll, beforeAll, beforeEach, expect, it, vi } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import type { Caller } from "#/modules/identity/actor";
import { FULL_GRANT } from "#/modules/identity/token-grant";
import { CloudStore } from "#/modules/config-store/store-sdk.server";
import { InngestClient } from "#/modules/inngest/client";
import { TAILSCALE } from "#/modules/private-access/private-access";
import { privateAccessUse } from "#/modules/private-access/private-access.commands.server";
import { setPrivateAccess } from "#/modules/private-access/private-access.server";
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
let unanswered = false;
const routers: ContainerObservation[] = [];
const placeRouter = (runtime: ContainerObservation["runtime"] = { state: "running", health: "healthy" }) => {
  routers.push({ ...runtimeWatchContainerFixture(SERVER, `router-${routers.length + 1}`), created_at_unix_nanos: Date.now() * 1e6, runtime });
};
const frame = () => runtimeWatchFrameFixture({
  machines: [runtimeWatchMachineObservationFixture({ machine: runtimeWatchMachineFixture(SERVER, "fra-1") })],
  services: [{ identity: TAILSCALE, service_id: "t".repeat(32) as ServiceId, containers: [...routers], hook_containers: [] }],
});
const session: ScopedRuntimeClientSession = {
  status: "connected",
  connected: asTestDouble<PloyzSession>()({
    watchFirstFrame: () => Effect.sync(frame),
    setPrivateAccess: (on: PrivateAccessOn | null) => Effect.sync(() => {
      handed.push(on);
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
  open: () => Effect.succeed<ScopedRuntimeClientSession>(session),
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

async function everyRow() {
  const tables = await harness.pool.query<{ name: string }>("select quote_ident(table_name) as name from information_schema.tables where table_schema = 'public' and table_type = 'BASE TABLE'");
  const rows = await Promise.all(tables.rows.map(({ name }) => harness.pool.query<{ row: string }>(`select row_to_json(t)::text as row from ${name} t`)));
  return rows.flatMap(({ rows: found }) => found.map(({ row }) => row)).join("\n");
}

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
  unanswered = false;
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

// Catches a turn-on from outside Ployz Cloud, which has no sealed key to send, or a turn-off that disconnects every router.
it("refuses turning it on or off from the CLI or an agent, and hands nothing to the Servers", async () => {
  for (const caller of [agent, { ...agent, via: "cli" as const }]) {
    for (const to of ["on", "off"] as const) {
      const asked = await run(privateAccessUse.invoke(caller, { to }, { approved: null, key: null }));
      expect(asked).toMatchObject({ ok: false, refusal: { code: "failed_precondition", message: "Turn Private Access on or off in Ployz Cloud, under Integrations." } });
    }
  }
  expect(handed).toEqual([]);
});

it("names a refused key apart from a Server that didn't answer", async () => {
  refuses = true;
  const changed = await run(setPrivateAccess(ORG, ON));
  expect(changed.failures.map(({ problem }) => problem)).toEqual(["key_refused"]);
  unanswered = true;
  const unreached = await run(setPrivateAccess(ORG, ON));
  expect(unreached.failures.map(({ problem }) => problem)).toEqual(["unreachable"]);
});
