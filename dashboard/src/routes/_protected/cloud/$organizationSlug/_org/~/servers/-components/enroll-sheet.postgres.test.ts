import { teardownJsdom } from "#/test/jsdom-globals";
import { generateKeyPairSync, sign } from "node:crypto";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createMemoryHistory, createRootRoute, createRouter, RouterProvider } from "@tanstack/react-router";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { Effect } from "effect";
import { Inngest } from "inngest";
import { createElement } from "react";
import { afterAll, afterEach, beforeAll, beforeEach, expect, it, vi } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { InngestClient } from "#/modules/inngest/client";
import type { HostReport } from "#/modules/machines/waiting/checkin-wire";
import { checkIn, checkinMessage } from "#/modules/machines/waiting/checkin.server";
import { previewServerEnrollment, readServerEnrollmentStatus } from "#/modules/machines/waiting/enroll.server";
import { createFleetToken } from "#/modules/machines/waiting/fleet-token.server";
import { SERVER_SILENT_AFTER_MS } from "#/modules/machines/waiting/servers-offline";
import { listWaitingServers } from "#/modules/machines/waiting/waiting-servers.server";
import * as waitingFunctions from "#/modules/machines/waiting/waiting.functions";
import * as memberFunctions from "#/modules/organization/members.functions";
import { ROLE_CHECKING } from "#/modules/organization/members.queries";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import { makePloyzLayer, type Ployz } from "#/modules/runtime/ployz.server";
import type { Database } from "#/server/database.server";
import type { SecretEncryption } from "#/utils/encrypted-secret.server";
import { machineId } from "#/test/ids";
import { type PostgresTestHarness, startPostgresTestHarness } from "#/test/postgres";
import { enrollStoreServer } from "#/test/store-cloud";
import { IntakeTray } from "./waiting-list";

const ada = { userId: "00000000-0000-4000-8000-000000000821" };
const adaOrg = "00000000-0000-4000-8000-000000000831";
const FRA_1 = machineId("1");
const FRA_2 = machineId("2");
const MINUTE = 60_000;

let harness: PostgresTestHarness;
const inngest = new Inngest({ id: "enroll-sheet-test" });
vi.spyOn(inngest, "send").mockResolvedValue({ ids: [] });
// Every dial Cloud makes to the Servers fails, as when the Runtime Watch reads them unreachable.
const unreachable = makePloyzLayer({ connect: async () => { throw new Error("Endpoint unavailable"); } });
const runtime = asTestDouble<OrganizationRuntime["Service"]>()({ cancel: () => Effect.void });

const run = <A, E>(operation: Effect.Effect<A, E, Database | SecretEncryption | InngestClient | OrganizationRuntime | Ployz>) =>
  harness.runEffect(operation.pipe(
    Effect.provideService(InngestClient, inngest),
    Effect.provideService(OrganizationRuntime, runtime),
    Effect.provide(unreachable),
  ));

const report: HostReport = {
  hostname: "web-2", os: { id: "ubuntu", version: "24.04" }, kernel: "6.8.0", arch: "x86_64", virt: "kvm", cpuCount: 4,
  memoryTotalBytes: 8_589_934_592, disks: [], docker: { kind: "absent" }, cgroup: "v2",
  zfs: { kind: "eligible", targets: [{ kind: "main_disk" }] }, blockers: [], warnings: [],
};

async function checkInHost() {
  const { token } = await run(createFleetToken(ada, { organizationSlug: "ada", name: "rack-1", expiresIn: "1d", autoEnroll: null }));
  const { publicKey, privateKey } = generateKeyPairSync("ed25519");
  const key = Buffer.from(publicKey.export({ format: "jwk" }).x ?? "", "base64url").toString("hex");
  const body = Buffer.from(JSON.stringify({ protocolVersion: 1, checkinKey: key, cliVersion: "0.3.0", report }));
  const timestamp = BigInt(Math.floor(Date.now() / 1000));
  const signature = sign(null, checkinMessage(key, timestamp, body), privateKey).toString("base64url");
  const headers = new Headers({ authorization: `Bearer ${token}`, "ployz-timestamp": String(timestamp), "ployz-signature": `v1=${signature}` });
  await run(checkIn({ headers, body, observedIp: "203.0.113.7" }));
}

beforeAll(async () => {
  harness = await startPostgresTestHarness();
}, 60_000);

afterAll(async () => {
  await harness.stop();
  teardownJsdom();
});

beforeEach(async () => {
  vi.stubGlobal("matchMedia", (query: string) => ({ matches: false, media: query, addEventListener() {}, removeEventListener() {} }));
  await harness.pool.query(`
    truncate table organization_pairing, organization_machine, observe_ingest_cursor, waiting_server, fleet_token, "user", organization cascade;
    insert into organization (id, name, slug) values ('${adaOrg}', 'Ada', 'ada');
    insert into "user" (id, email, name) values ('${ada.userId}', 'ada@example.test', 'Ada');
    insert into member (id, organization_id, user_id, role, created_at) values (gen_random_uuid(), '${adaOrg}', '${ada.userId}', 'owner', now());
  `);
  await run(enrollStoreServer(adaOrg, { secret: "ppair_enroll_sheet", server: FRA_1 }));
  await harness.pool.query("update organization_pairing set founder_machine_id = $1", [FRA_1]);
  await harness.pool.query(`insert into organization_machine (organization_id, machine_id, cluster_key, encrypted_capability)
    select organization_id, $1, cluster_key, encrypted_capability from organization_machine`, [FRA_2]);
  await harness.pool.query("update organization_machine set created_at = now() - interval '30 days', push_target = 'https://cloud.example.com/observe'");
  await checkInHost();
  vi.spyOn(waitingFunctions, "listWaitingServersServerFn").mockImplementation(({ data }) => run(listWaitingServers(ada, data)));
  vi.spyOn(waitingFunctions, "serverEnrollmentStatusServerFn").mockImplementation(({ data }) => run(readServerEnrollmentStatus(ada, data)));
  vi.spyOn(waitingFunctions, "planServerEnrollmentServerFn").mockImplementation(({ data }) => run(previewServerEnrollment(ada, data)));
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

type Members = Awaited<ReturnType<typeof memberFunctions.listMembersServerFn>>;
const adaOwns: Members = {
  members: [{ you: true, role: "owner", name: "Ada", email: "ada@example.test", joined_at: "2026-10-01T00:00:00Z" }],
  invites: [],
  mail: false,
  support: false,
};

const pushedAgo = (machine: string, ms: number) => harness.pool.query(
  "insert into observe_ingest_cursor (organization_id, machine_id, stream, seq, updated_at) values ($1, $2, 'crash', 1, now() - $3 * interval '1 millisecond')",
  [adaOrg, machine, ms],
);

/**
 * Opens the Enroll sheet for the waiting host from the Servers page's tray, and waits for its plan. Enroll stays closed
 * while the read of Ada's role is in flight, and opens once it names her the owner.
 */
async function openEnroll() {
  let answerRole = (_members: Members) => {};
  vi.spyOn(memberFunctions, "listMembersServerFn").mockReturnValue(new Promise((resolve) => { answerRole = resolve; }));
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const router = createRouter({
    routeTree: createRootRoute({
      component: () => createElement(QueryClientProvider, { client }, createElement(IntakeTray, { organizationSlug: "ada", token: undefined, firstRun: false })),
    }),
    history: createMemoryHistory(),
  });
  await router.load();
  render(createElement(RouterProvider, { router }));
  fireEvent.click(await screen.findByRole("checkbox", { name: "Select web-2" }, { timeout: 5000 }));
  const pick = screen.getByRole<HTMLButtonElement>("button", { name: "Enroll 1" });
  expect({ disabled: pick.disabled, title: pick.title }).toEqual({ disabled: true, title: ROLE_CHECKING });
  answerRole(adaOwns);
  await waitFor(() => expect(pick.disabled).toBe(false), { timeout: 5000 });
  fireEvent.click(pick);
  await vi.waitFor(() => expect(screen.getByRole("region", { name: "Enrollment preview" }).textContent).toContain("web-2"), { timeout: 5000 });
  const enroll = screen.getAllByRole<HTMLButtonElement>("button", { name: "Enroll 1" }).at(-1);
  return { alert: screen.queryByRole("alert")?.textContent ?? null, enrollDisabled: enroll?.disabled };
}

it("warns with Forget Servers once every Server's last push is past the window, and still lets you enroll", async () => {
  await pushedAgo(FRA_1, SERVER_SILENT_AFTER_MS + MINUTE);
  await pushedAgo(FRA_2, SERVER_SILENT_AFTER_MS + 60 * MINUTE);
  expect(await openEnroll()).toEqual({ alert: "Servers offlineForget them first. Forget Servers", enrollDisabled: false });
  expect(screen.getByRole("link", { name: "Forget Servers" }).getAttribute("href")).toBe("/cloud/ada/~/settings#organization-teardown");
}, 15_000);

it("doesn't warn while one Server pushed recently and the other is past the window", async () => {
  await pushedAgo(FRA_1, 2 * MINUTE);
  await pushedAgo(FRA_2, SERVER_SILENT_AFTER_MS + MINUTE);
  expect(await openEnroll()).toEqual({ alert: null, enrollDisabled: false });
}, 15_000);

it("doesn't warn on a recent push while every dial to the Servers fails", async () => {
  await pushedAgo(FRA_1, 2 * MINUTE);
  await pushedAgo(FRA_2, 2 * MINUTE);
  expect(await openEnroll()).toEqual({ alert: null, enrollDisabled: false });
}, 15_000);
