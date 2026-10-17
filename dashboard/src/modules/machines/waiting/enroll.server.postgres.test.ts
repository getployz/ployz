import { createHash, generateKeyPairSync, type KeyObject, sign } from "node:crypto";
import { Cause, Effect, Exit, Schema } from "effect";
import { Inngest } from "inngest";
import { afterAll, beforeAll, beforeEach, expect, it, vi } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { CloudStore, openCloudStore } from "#/modules/config-store/store-sdk.server";
import { InngestClient } from "#/modules/inngest/client";
import { serverEnrollmentReportedEvent } from "#/modules/inngest/events";
import { rustMachineIdSchema } from "#/modules/machines/enrollment";
import { forgetServers } from "#/modules/machines/forget-servers.server";
import { enrollMachine, hashEnrollmentToken, insertEnrollmentToken } from "#/modules/machines/enrollment.server";
import { forgetEmptiedPairing, revokeOrganizationPairing, seeCluster } from "#/modules/machines/pairing-removal.server";
import type { CheckinRequest, HostReport, Prep } from "#/modules/machines/waiting/checkin-wire";
import { checkIn, checkinMessage } from "#/modules/machines/waiting/checkin.server";
import {
  cancelServerEnrollment,
  readServerEnrollments,
  startServerEnrollment,
} from "#/modules/machines/waiting/enroll.server";
import { bindServerEnrollment, closeServerEnrollment, closeStaleServerEnrollments, releaseJoiner } from "#/modules/machines/waiting/enrollment-run.server";
import { createFleetToken, revokeFleetToken } from "#/modules/machines/waiting/fleet-token.server";
import { openProbeSelection } from "#/modules/machines/waiting/probe.server";
import { createServerEnrollmentRun, executeServerEnrollment } from "#/modules/machines/waiting/server-enrollment.inngest";
import { rejectWaitingServers } from "#/modules/machines/waiting/reject.server";
import { type EnrollInput, ZFS_BUILD_FAILED } from "#/modules/machines/waiting/server-enrollment";
import { removeOrganization } from "#/modules/organization/organization-removal.server";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import { makePloyzLayer, type Ployz } from "#/modules/runtime/ployz.server";
import type { Database } from "#/server/database.server";
import { makeInngestEffectRunner, type runInngestEffect } from "#/server/run.server";
import { makeSecretEncryption, type SecretEncryption } from "#/utils/encrypted-secret.server";
import { invokeOnFailure } from "#/test/inngest-failure";
import { type PostgresTestHarness, startPostgresTestHarness } from "#/test/postgres";

const ada = { userId: "00000000-0000-4000-8000-000000000801" };
const adaOrg = "00000000-0000-4000-8000-000000000811";

let harness: PostgresTestHarness;
const inngest = new Inngest({ id: "enroll-test" });
const sent = vi.spyOn(inngest, "send").mockResolvedValue({ ids: [] });

const unreachable = makePloyzLayer({ connect: async () => { throw new Error("Endpoint unavailable"); } });
const runtime = asTestDouble<OrganizationRuntime["Service"]>()({ cancel: () => Effect.void });
type Needs = Database | SecretEncryption | InngestClient | OrganizationRuntime | Ployz;

const run = <A, E>(operation: Effect.Effect<A, E, Needs>) =>
  harness.runEffect(operation.pipe(
    Effect.provideService(InngestClient, inngest),
    Effect.provideService(OrganizationRuntime, runtime),
    Effect.provide(unreachable),
  ));

const failure = async <A, E>(operation: Effect.Effect<A, E, Needs>) => {
  const exit = await run(Effect.exit(operation));
  return Exit.isFailure(exit) ? Cause.squash(exit.cause) : null;
};

beforeAll(async () => {
  harness = await startPostgresTestHarness();
}, 60_000);

afterAll(async () => {
  await harness.stop();
});

let fleet: string;

beforeEach(async () => {
  sent.mockClear();
  await harness.pool.query(`
    truncate table operation_outbox, server_enrollment, machine_enrollment_token, organization_pairing,
      checkin_rate_limit, waiting_server, fleet_token, "user", organization cascade;
    insert into organization (id, name, slug) values ('${adaOrg}', 'Ada', 'ada');
    insert into "user" (id, email, name) values ('${ada.userId}', 'ada@example.test', 'Ada');
    insert into member (id, organization_id, user_id, role, created_at) values
      (gen_random_uuid(), '${adaOrg}', '${ada.userId}', 'owner', now());
  `);
  ({ token: fleet } = await run(
    createFleetToken(ada, { organizationSlug: "ada", name: "rack-1", expiresIn: "1d", autoEnroll: null }),
  ));
});

const report: HostReport = {
  hostname: "web",
  os: { id: "ubuntu", version: "24.04" },
  kernel: "6.8.0",
  arch: "x86_64",
  virt: "kvm",
  cpuCount: 4,
  memoryTotalBytes: 8_589_934_592,
  disks: [],
  docker: { kind: "absent" },
  cgroup: "v2",
  zfs: { kind: "eligible", targets: [{ kind: "main_disk" }] },
  blockers: [],
  warnings: [],
};

type Host = { readonly name: string; readonly key: string; readonly privateKey: KeyObject; id?: string };

const host = (name: string): Host => {
  const { publicKey, privateKey } = generateKeyPairSync("ed25519");
  const x = publicKey.export({ format: "jwk" }).x ?? "";
  return { name, key: Buffer.from(x, "base64url").toString("hex"), privateKey };
};

const checkin = async (target: Host, prep?: Prep) => {
  const base = { protocolVersion: 1, checkinKey: target.key, cliVersion: "0.3.1", report: { ...report, hostname: target.name } } as const;
  const body = Buffer.from(JSON.stringify(prep === undefined ? base : { ...base, prep } satisfies CheckinRequest));
  const timestamp = BigInt(Math.floor(Date.now() / 1000));
  const signature = sign(null, checkinMessage(target.key, timestamp, body), target.privateKey).toString("base64url");
  const headers = new Headers({
    authorization: `Bearer ${fleet}`,
    "ployz-timestamp": String(timestamp),
    "ployz-signature": `v1=${signature}`,
  });
  const answer = await run(checkIn({ headers, body, observedIp: "203.0.113.7" }));
  const { rows } = await harness.pool.query<{ id: string }>("select id from waiting_server where checkin_key = $1", [target.key]);
  target.id = rows[0]?.id;
  return answer;
};

const waiting = async (...names: string[]) => {
  const hosts = names.map(host);
  for (const each of hosts) await checkin(each);
  return hosts;
};

const enroll = (hosts: ReadonlyArray<Host>, overrides: Partial<EnrollInput> = {}) =>
  startServerEnrollment(ada, {
    organizationSlug: "ada",
    ids: hosts.map((each) => each.id ?? ""),
    storage: { kind: "none" },
    names: { kind: "hostnames" },
    runs: { services: true, ingress: true, builds: false },
    ...overrides,
  });

type AttemptRow = { id: string; role: string; state: string; ended_at: Date | null; inngest_run_id: string | null };
const attempts = async () =>
  (await harness.pool.query<AttemptRow>("select * from server_enrollment order by requested_at, role desc")).rows;
const outbox = async () =>
  (await harness.pool.query<{ name: string }>("select name from operation_outbox order by created_at, name")).rows.map((row) => row.name);

it("starts the first server and its joiners, each on its own role-bound token, with one run each", async () => {
  const [a, b] = await waiting("web-a", "web-b");
  if (a === undefined || b === undefined) throw new Error("two hosts");
  const started = await run(enroll([a, b], { first: b.id }));
  expect(started.attempts.map((attempt) => [attempt.waitingServerId, attempt.role, attempt.state])).toEqual([
    [b.id, "founder", "instructed"],
    [a.id, "joiner", "awaiting_founder"],
  ]);
  const tokens = await harness.pool.query<{ enrollment_role: string; server_enrollment_id: string }>(
    "select enrollment_role, server_enrollment_id from machine_enrollment_token order by enrollment_role",
  );
  expect(tokens.rows).toEqual([
    { enrollment_role: "founder", server_enrollment_id: started.attempts[0]?.id },
    { enrollment_role: "joiner", server_enrollment_id: started.attempts[1]?.id },
  ]);
  expect(await outbox()).toEqual(["server/enrollment.requested", "server/enrollment.requested"]);
});

it("joins servers added while the first server is on its way, and skips a host already enrolling", async () => {
  const [a, b, c] = await waiting("web-a", "web-b", "web-c");
  if (a === undefined || b === undefined || c === undefined) throw new Error("three hosts");
  await run(enroll([a]));
  const meanwhile = await run(enroll([a, b]));
  expect(meanwhile.skipped).toEqual([{ id: a.id, reason: "Already enrolling" }]);
  expect(meanwhile.attempts.map((attempt) => [attempt.waitingServerId, attempt.role, attempt.state])).toEqual([
    [b.id, "joiner", "awaiting_founder"],
  ]);
  expect(await claimAsFounder(a, "1".repeat(32))).toMatchObject({ kind: "initialize" });
  const paired = await run(enroll([c]));
  expect(paired.attempts.map((attempt) => [attempt.waitingServerId, attempt.role])).toEqual([[c.id, "joiner"]]);
});

it("holds one open attempt per host, and one open first server per Organization, at the database", async () => {
  const [a, b] = await waiting("web-a", "web-b");
  if (a === undefined || b === undefined) throw new Error("two hosts");
  await run(enroll([a]));
  const copy = (waitingServerId: string, role: string) => harness.pool.query(
    `insert into server_enrollment (organization_id, waiting_server_id, fleet_token_id, role, instruction, enroll_token_id, state)
     select organization_id, $1, fleet_token_id, $2, instruction, enroll_token_id, 'instructed' from server_enrollment limit 1`,
    [waitingServerId, role],
  );
  await expect(copy(a.id ?? "", "joiner")).rejects.toThrow(/server_enrollment_one_open_idx/u);
  await expect(copy(b.id ?? "", "founder")).rejects.toThrow(/server_enrollment_one_founder_idx/u);
  await harness.pool.query("update server_enrollment set ended_at = now(), state = 'cancelled'");
  await expect(copy(a.id ?? "", "founder")).resolves.toBeDefined();
});

it("answers enroll once the host is ready for its storage, and wait before then", async () => {
  const [a] = await waiting("web-a");
  if (a === undefined) throw new Error("one host");
  const [started] = (await run(enroll([a], { storage: { kind: "zfs", useSpareDisk: false, fallbackToDocker: false } }))).attempts;
  expect(await checkin(a, { docker: "ready", zfs: "installing" })).toEqual({ kind: "wait", retryAfter: 10 });
  expect((await run(readServerEnrollments(ada, { organizationSlug: "ada" })))[0]?.queued).toBe(true);

  const answer = await checkin(a, { docker: "ready", zfs: "ready" });
  expect(answer).toMatchObject({ kind: "enroll", attemptId: started?.id, role: "founder", name: "web-a", storage: { kind: "zfs" } });
  const token = answer.kind === "enroll" ? answer.token : "";
  const issued = await harness.pool.query("select 1 from machine_enrollment_token where token_hash = $1 and server_enrollment_id = $2", [
    hashEnrollmentToken(token),
    started?.id,
  ]);
  expect(issued.rowCount).toBe(1);
  expect((await run(readServerEnrollments(ada, { organizationSlug: "ada" })))[0]?.queued).toBe(false);
});

it("keeps a joiner waiting until its founder is in", async () => {
  const [a, b] = await waiting("web-a", "web-b");
  if (a === undefined || b === undefined) throw new Error("two hosts");
  await run(enroll([a, b], { first: a.id }));
  expect(await checkin(b)).toEqual({ kind: "wait", retryAfter: 10 });
  await harness.pool.query("update server_enrollment set state = 'instructed' where role = 'joiner'");
  expect(await checkin(b)).toMatchObject({ kind: "enroll", role: "joiner", name: "web-b" });
});

it("reports a failed ZFS build to the run and stops answering enroll", async () => {
  const [a] = await waiting("web-a");
  if (a === undefined) throw new Error("one host");
  const [started] = (await run(enroll([a], { storage: { kind: "zfs", useSpareDisk: false, fallbackToDocker: false } }))).attempts;
  sent.mockClear();
  expect(await checkin(a, { docker: "ready", zfs: "failed" })).toEqual({ kind: "wait", retryAfter: 10 });
  expect(sent).toHaveBeenCalledWith(expect.objectContaining({
    name: serverEnrollmentReportedEvent,
    data: { attemptId: started?.id, stage: "storage", failure: { stage: "storage", message: ZFS_BUILD_FAILED } },
  }));
});

it("ignores a stale failed ZFS build once the host is past storage", async () => {
  const [a] = await waiting("web-a");
  if (a === undefined) throw new Error("one host");
  await run(enroll([a], { storage: { kind: "zfs", useSpareDisk: false, fallbackToDocker: false } }));
  await harness.pool.query("update server_enrollment set state = 'running', stage = 'joining'");
  sent.mockClear();
  expect(await checkin(a, { docker: "ready", zfs: "failed" })).toMatchObject({ kind: "enroll", role: "founder" });
  expect(sent).not.toHaveBeenCalled();
});

it("keeps an enrolling host's row and check-ins through a revoke, and removes the rest", async () => {
  const [a, b] = await waiting("web-a", "web-b");
  if (a === undefined || b === undefined) throw new Error("two hosts");
  await run(enroll([a]));
  const [token] = (await harness.pool.query<{ id: string }>("select id from fleet_token")).rows;
  expect(await run(revokeFleetToken(ada, { organizationSlug: "ada", id: token?.id ?? "" }))).toEqual({ waitingRemoved: 1 });
  expect(await checkin(a)).toMatchObject({ kind: "enroll", name: "web-a" });
  expect(await checkin(b)).toMatchObject({ kind: "stop", reason: "token_revoked" });
});

it("cancels a rejected host's open attempt", async () => {
  const [a] = await waiting("web-a");
  if (a === undefined) throw new Error("one host");
  await run(enroll([a]));
  await run(rejectWaitingServers(ada, { organizationSlug: "ada", ids: [a.id ?? ""] }));
  expect((await attempts()).map((row) => [row.state, row.ended_at !== null])).toEqual([["cancelled", true]]);
  expect(await outbox()).toContain("server/enrollment.cancel");
});

const encryption = makeSecretEncryption("test-encryption-secret");
const legacyMachine = "00000000000000000000000000000001";
const legacyClaim = (age: string) =>
  harness.pool.query(
    "insert into organization_pairing (organization_id, encrypted_pairing_secret, founder_public_key, founder_claim_machine_id, created_at) values ($1, $2, 'k', $3, now() - $4::interval)",
    [adaOrg, encryption.encrypt("ployz1:legacy-pairing"), legacyMachine, age],
  );
const pairings = async () =>
  (await harness.pool.query<{ founder_claim_machine_id: string }>("select founder_claim_machine_id from organization_pairing")).rows;
const ageClaim = () => harness.pool.query("update organization_pairing set created_at = now() - interval '1 hour'");

const identity = (machineId: string) => ({
  protocolVersion: 1 as const,
  machineId: Schema.decodeUnknownSync(rustMachineIdSchema)(machineId),
  initialPolicy: { labels: {}, accepts_builds: true, accepts_services: true, accepts_ingress: true },
  name: `server-${machineId.slice(0, 4)}`,
  publicKey: createHash("sha256").update(machineId).digest("base64"),
  advertisedEndpoints: ["10.0.0.9:51820"],
  requestedStorage: "none" as const,
});

const claimAsFounder = async (target: Host, machineId: string) => {
  const answer = await checkin(target);
  if (answer.kind !== "enroll") throw new Error(`expected enroll, got ${answer.kind}`);
  return run(enrollMachine({ token: answer.token, identity: identity(machineId) }));
};

const serverAdd = async (machineId: string) => {
  const legacy = await run(insertEnrollmentToken({ organizationId: adaOrg, userId: ada.userId }));
  return run(enrollMachine({ token: legacy.token, identity: identity(machineId) }));
};

const failedFounder = async (target: Host, machineId: string) => {
  const [attempt] = (await run(enroll([target]))).attempts;
  expect(await claimAsFounder(target, machineId)).toMatchObject({ kind: "initialize", resumed: false });
  await run(closeServerEnrollment("unbound", "failure", attempt?.id));
};

it("plans a first server over an old founding claim no token can finish, and drops that claim", async () => {
  const [a, b] = await waiting("web-a", "web-b");
  if (a === undefined || b === undefined) throw new Error("two hosts");
  await legacyClaim("1 hour");
  const started = await run(enroll([a, b], { first: a.id }));
  expect(started.attempts.map((attempt) => [attempt.waitingServerId, attempt.role, attempt.state])).toEqual([
    [a.id, "founder", "instructed"],
    [b.id, "joiner", "awaiting_founder"],
  ]);
  expect(await pairings()).toEqual([]);
});

it("keeps an aged founding claim from server add while its token can still enroll, and plans only joiners", async () => {
  const [a, b] = await waiting("web-a", "web-b");
  if (a === undefined || b === undefined) throw new Error("two hosts");
  expect(await serverAdd("2".repeat(32))).toMatchObject({ kind: "initialize", resumed: false });
  await ageClaim();
  const started = await run(enroll([a, b], { first: a.id }));
  expect(started.attempts.map((attempt) => [attempt.role, attempt.state])).toEqual([
    ["joiner", "awaiting_founder"],
    ["joiner", "awaiting_founder"],
  ]);
  expect(await pairings()).toEqual([{ founder_claim_machine_id: "2".repeat(32) }]);
});

it("keeps an aged founding claim from before Cloud recorded its token while its Machine holds a live token", async () => {
  const [a] = await waiting("web-a");
  if (a === undefined) throw new Error("one host");
  await legacyClaim("1 hour");
  await run(insertEnrollmentToken({ organizationId: adaOrg, userId: ada.userId }));
  await harness.pool.query("update machine_enrollment_token set claimed_machine_id = $1, claimed_public_key = 'k', claimed_at = now()", [legacyMachine]);
  expect((await run(enroll([a]))).attempts.map((attempt) => attempt.role)).toEqual(["joiner"]);
  expect(await pairings()).toEqual([{ founder_claim_machine_id: legacyMachine }]);
});

it("keeps a founding claim server add resumed after the same Machine's attempt failed", async () => {
  const [a, b] = await waiting("previous-founder", "new-host");
  if (a === undefined || b === undefined) throw new Error("two hosts");
  await failedFounder(a, "1".repeat(32));
  expect(await serverAdd("1".repeat(32))).toMatchObject({ kind: "initialize", resumed: true });
  const before = await pairings();
  expect((await run(enroll([b]))).attempts.map((attempt) => attempt.role)).toEqual(["joiner"]);
  expect(await pairings()).toEqual(before);
});

it("drops a founding claim whose attempt failed and plans a new first server", async () => {
  const [a, b] = await waiting("web-a", "web-b");
  if (a === undefined || b === undefined) throw new Error("two hosts");
  await failedFounder(a, "1".repeat(32));
  const started = await run(enroll([b]));
  expect(started.attempts.map((attempt) => [attempt.waitingServerId, attempt.role])).toEqual([[b.id, "founder"]]);
  expect(await pairings()).toEqual([]);
});

it("keeps a dead claim's published Server pending removal, and refuses to plan until Cloud reaches it", async () => {
  const [a, b] = await waiting("web-a", "web-b");
  if (a === undefined || b === undefined) throw new Error("two hosts");
  const machineId = "1".repeat(32);
  const [attempt] = (await run(enroll([a]))).attempts;
  await claimAsFounder(a, machineId);
  await harness.pool.query(
    "insert into organization_machine (organization_id, machine_id, cluster_key, encrypted_capability, is_dial_entry) values ($1, $2, $3, $4, true)",
    [adaOrg, machineId, "a".repeat(64), encryption.encrypt("ployz1:published")],
  );
  await run(closeServerEnrollment("unbound", "failure", attempt?.id));
  expect(await failure(enroll([b]))).toMatchObject({ _tag: "Conflict" });
  const { rows } = await harness.pool.query<{ removal_endpoints: Array<{ machineId: string; status: string }> }>(
    "select removal_endpoints from organization_pairing",
  );
  expect(rows[0]?.removal_endpoints.map((endpoint) => [endpoint.machineId, endpoint.status])).toEqual([[machineId, "pending"]]);
  expect((await attempts()).map((row) => row.state)).toEqual(["failed"]);
});

it("sends Enroll to Forget Servers when the earlier pairing waits only on Machines Cloud can't reach", async () => {
  const [a, b] = await waiting("web-a", "web-b");
  if (a === undefined || b === undefined) throw new Error("two hosts");
  await run(enroll([a]));
  await claimAsFounder(a, "1".repeat(32));
  await harness.pool.query(
    "update organization_pairing set removal_started_at = date_trunc('milliseconds', now()), removal_endpoints = $1::jsonb",
    [JSON.stringify([{ machineId: "1".repeat(32), status: "unknown" }])],
  );
  expect(await failure(enroll([b]))).toMatchObject({ _tag: "Conflict", message: "Cloud can't remove an earlier server's pairing. Use Forget Servers." });
});

it("lets a host whose founding attempt failed found again on a new enrollment", async () => {
  const [a] = await waiting("web-a");
  if (a === undefined) throw new Error("one host");
  const [first] = (await run(enroll([a]))).attempts;
  expect(await claimAsFounder(a, "1".repeat(32))).toMatchObject({ kind: "initialize", resumed: false });
  await run(closeServerEnrollment("unbound", "failure", first?.id));

  const [again] = (await run(enroll([a]))).attempts;
  expect([again?.role, again?.state]).toEqual(["founder", "instructed"]);
  expect(await claimAsFounder(a, "1".repeat(32))).toMatchObject({ kind: "initialize", resumed: false });
});

it("ends open attempts when the pairing is removed", async () => {
  const [a] = await waiting("web-a");
  if (a === undefined) throw new Error("one host");
  await run(enroll([a]));
  await claimAsFounder(a, "1".repeat(32));
  await run(revokeOrganizationPairing(adaOrg));
  expect((await attempts()).map((row) => [row.state, row.ended_at !== null])).toEqual([["cancelled", true]]);
  expect(await checkin(a)).toEqual({ kind: "wait", retryAfter: 10 });
});

it("ends open attempts when Cloud forgets an emptied pairing", async () => {
  const [a] = await waiting("web-a");
  if (a === undefined) throw new Error("one host");
  await run(enroll([a]));
  await claimAsFounder(a, "1".repeat(32));
  const { seen } = await run(seeCluster(adaOrg));
  expect(await run(forgetEmptiedPairing(adaOrg, seen.generation ?? ""))).toEqual({ kind: "released" });
  expect((await attempts()).map((row) => [row.state, row.ended_at !== null])).toEqual([["cancelled", true]]);
});

it("fails an attempt whose enroll token was removed under it", async () => {
  const [a] = await waiting("forgotten-host");
  if (a === undefined) throw new Error("one host");
  const [started] = (await run(enroll([a]))).attempts;
  await harness.pool.query("delete from machine_enrollment_token where organization_id = $1", [adaOrg]);
  await executeServerEnrollment({
    event: { data: { attemptId: started?.id ?? "", organizationId: adaOrg } },
    runId: "run-forgotten",
    step: asTestDouble<Parameters<typeof executeServerEnrollment>[0]["step"]>()({
      run: async <A>(_name: string, fn: () => Promise<A>) => fn(),
      waitForEvent: async () => null,
      sendEvent: async () => undefined,
    }),
  }, makeInngestEffectRunner(run) as typeof runInngestEffect);
  expect((await attempts()).map((row) => [row.state, row.ended_at !== null])).toEqual([["failed", true]]);
});

it("waits for a reject in flight before planning, and skips the host it rejected", async () => {
  const [a, b] = await waiting("web-a", "web-b");
  if (a === undefined || b === undefined) throw new Error("two hosts");
  const rejecting = await harness.pool.connect();
  try {
    await rejecting.query("begin");
    await rejecting.query("update waiting_server set rejected_at = now() where id = $1", [a.id]);
    const planning = run(enroll([a, b], { first: b.id }));
    await new Promise((resolve) => setTimeout(resolve, 300));
    await rejecting.query("commit");
    const started = await planning;
    expect(started.attempts.map((attempt) => attempt.waitingServerId)).toEqual([b.id]);
    expect(started.skipped).toEqual([{ id: a.id, reason: "Not waiting" }]);
  } finally {
    rejecting.release();
  }
});

it("leaves an attempt open when revoking its enroll token fails", async () => {
  const [a] = await waiting("web-a");
  if (a === undefined) throw new Error("one host");
  const [started] = (await run(enroll([a]))).attempts;
  await harness.pool.query(`
    create function refuse_token_update() returns trigger language plpgsql as $$ begin raise exception 'token store down'; end $$;
    create trigger refuse_token_update before update on machine_enrollment_token for each row execute function refuse_token_update();
  `);
  try {
    expect(await failure(closeServerEnrollment("run-1", "cancellation", started?.id))).not.toBeNull();
  } finally {
    await harness.pool.query("drop trigger refuse_token_update on machine_enrollment_token; drop function refuse_token_update();");
  }
  expect((await attempts()).map((row) => [row.state, row.ended_at])).toEqual([["instructed", null]]);
});

it("releases a joiner again when a retried release step finds it already released", async () => {
  const [a, b] = await waiting("web-a", "web-b");
  if (a === undefined || b === undefined) throw new Error("two hosts");
  const joiner = (await run(enroll([a, b], { first: a.id }))).attempts[1];
  await harness.pool.query("update server_enrollment set inngest_run_id = 'run-1'");
  expect(await run(releaseJoiner(joiner?.id ?? "", "run-1"))).toBe(true);
  expect(await run(releaseJoiner(joiner?.id ?? "", "run-1"))).toBe(true);
  expect(await run(releaseJoiner(joiner?.id ?? "", "run-2"))).toBe(false);
});

it("closes a cancelled attempt no run has bound, and says when there was nothing to cancel", async () => {
  const [a] = await waiting("web-a");
  if (a === undefined) throw new Error("one host");
  const [started] = (await run(enroll([a]))).attempts;
  const id = started?.id ?? "";
  expect(await run(cancelServerEnrollment(ada, { organizationSlug: "ada", id }))).toEqual({ id, cancelling: true });
  expect((await attempts()).map((row) => row.state)).toEqual(["cancelled"]);
  expect(await checkin(a)).toEqual({ kind: "wait", retryAfter: 10 });
  expect(await run(cancelServerEnrollment(ada, { organizationSlug: "ada", id }))).toEqual({ id, cancelling: false });
  expect(await failure(cancelServerEnrollment(ada, { organizationSlug: "ada", id: "00000000-0000-4000-8000-000000000899" })))
    .toMatchObject({ _tag: "NotFound" });
});

it("fails an attempt whose run failed before it bound the attempt", async () => {
  const [a] = await waiting("web-a");
  if (a === undefined) throw new Error("one host");
  const [started] = (await run(enroll([a]))).attempts;
  const runner = createServerEnrollmentRun(new Inngest({ id: "test" }), makeInngestEffectRunner(run) as typeof runInngestEffect);
  await invokeOnFailure(runner, "run-never-bound", {
    name: "server/enrollment.requested",
    data: { attemptId: started?.id, organizationId: adaOrg },
  });
  expect((await attempts()).map((row) => [row.state, row.ended_at !== null, row.inngest_run_id])).toEqual([["failed", true, null]]);
});

it("keeps a founding claim while an earlier token for its Machine can still enroll", async () => {
  const [a] = await waiting("new-host");
  if (a === undefined) throw new Error("one host");
  const machineId = "3".repeat(32);
  expect(await serverAdd(machineId)).toMatchObject({ kind: "initialize", resumed: false });
  const resumed = await run(insertEnrollmentToken({ organizationId: adaOrg, userId: ada.userId }));
  expect(await run(enrollMachine({ token: resumed.token, identity: identity(machineId) }))).toMatchObject({ kind: "initialize", resumed: true });
  await harness.pool.query("update machine_enrollment_token set expires_at = now() - interval '1 second' where id = $1", [resumed.id]);
  expect((await run(enroll([a]))).attempts.map((attempt) => attempt.role)).toEqual(["joiner"]);
  expect(await pairings()).toEqual([{ founder_claim_machine_id: machineId }]);
});

it("keeps a server add resume live after the Machine's Waiting token retries and its attempt is cancelled", async () => {
  const [a, b] = await waiting("original-host", "replacement-host");
  if (a === undefined || b === undefined) throw new Error("two hosts");
  const machineId = "4".repeat(32);
  const [attempt] = (await run(enroll([a]))).attempts;
  const instruction = await checkin(a);
  if (instruction.kind !== "enroll") throw new Error("expected instruction");
  expect(await run(enrollMachine({ token: instruction.token, identity: identity(machineId) }))).toMatchObject({ kind: "initialize", resumed: false });
  expect(await serverAdd(machineId)).toMatchObject({ kind: "initialize", resumed: true });
  expect(await run(enrollMachine({ token: instruction.token, identity: identity(machineId) }))).toMatchObject({ kind: "initialize", resumed: true });
  await run(cancelServerEnrollment(ada, { organizationSlug: "ada", id: attempt?.id ?? "" }));
  expect((await run(enroll([b]))).attempts.map((each) => each.role)).toEqual(["joiner"]);
  expect(await pairings()).toEqual([{ founder_claim_machine_id: machineId }]);
});

it("drops a dead server add claim behind an open first server, which then founds", async () => {
  const [a, b] = await waiting("planned-founder", "later-host");
  if (a === undefined || b === undefined) throw new Error("two hosts");
  const [founder] = (await run(enroll([a]))).attempts;
  const spare = await run(insertEnrollmentToken({ organizationId: adaOrg, userId: ada.userId }));
  expect(await serverAdd("5".repeat(32))).toMatchObject({ kind: "initialize", resumed: false });
  await harness.pool.query("update machine_enrollment_token set expires_at = now() - interval '1 second' where claimed_machine_id = $1", ["5".repeat(32)]);
  expect(await claimAsFounder(a, "6".repeat(32))).toMatchObject({ kind: "not_yet" });
  const later = await run(enroll([b]));
  expect(later.attempts.map((attempt) => [attempt.role, attempt.state])).toEqual([["joiner", "awaiting_founder"]]);
  expect(await pairings()).toEqual([]);
  expect((await attempts()).find((row) => row.id === founder?.id)).toMatchObject({ state: "instructed", ended_at: null });
  expect(await claimAsFounder(a, "6".repeat(32))).toMatchObject({ kind: "initialize", resumed: false });
  const { rows } = await harness.pool.query("select 1 from machine_enrollment_token where id = $1", [spare.id]);
  expect(rows).toHaveLength(1);
});

it("ends an open first server when the dead claim's published Server can't be reached", async () => {
  const [a, b] = await waiting("planned-founder", "later-host");
  if (a === undefined || b === undefined) throw new Error("two hosts");
  const [founder] = (await run(enroll([a]))).attempts;
  const machineId = "5".repeat(32);
  expect(await serverAdd(machineId)).toMatchObject({ kind: "initialize", resumed: false });
  await harness.pool.query(
    "insert into organization_machine (organization_id, machine_id, cluster_key, encrypted_capability, is_dial_entry) values ($1, $2, $3, $4, true)",
    [adaOrg, machineId, "a".repeat(64), encryption.encrypt("ployz1:published")],
  );
  await harness.pool.query("update machine_enrollment_token set expires_at = now() - interval '1 second' where claimed_machine_id = $1", [machineId]);
  expect(await failure(enroll([b]))).toMatchObject({ _tag: "Conflict" });
  expect((await attempts()).map((row) => [row.id, row.state])).toEqual([[founder?.id, "cancelled"]]);
  expect(await outbox()).toContain("server/enrollment.cancel");
});

it("clears the Organization's tokens when a full removal resumes a stuck dead-claim release", async () => {
  const [b] = await waiting("later-host");
  if (b === undefined) throw new Error("one host");
  const machineId = "5".repeat(32);
  expect(await serverAdd(machineId)).toMatchObject({ kind: "initialize", resumed: false });
  await harness.pool.query(
    "insert into organization_machine (organization_id, machine_id, cluster_key, encrypted_capability, is_dial_entry) values ($1, $2, $3, $4, true)",
    [adaOrg, machineId, "a".repeat(64), encryption.encrypt("ployz1:published")],
  );
  await harness.pool.query("update machine_enrollment_token set expires_at = now() - interval '1 second' where claimed_machine_id = $1", [machineId]);
  const spare = await run(insertEnrollmentToken({ organizationId: adaOrg, userId: ada.userId }));
  expect(await failure(enroll([b]))).toMatchObject({ _tag: "Conflict" });
  expect((await harness.pool.query("select 1 from machine_enrollment_token where id = $1", [spare.id])).rows).toHaveLength(1);
  expect(await run(revokeOrganizationPairing(adaOrg))).toMatchObject({ confirmed: false });
  expect((await harness.pool.query("select 1 from machine_enrollment_token")).rows).toEqual([]);
});

const waitFor = async (event: string) => {
  for (let i = 0; i < 100; i++) {
    const { rows } = await harness.pool.query("select 1 from pg_stat_activity where datname = current_database() and wait_event = $1", [event]);
    if (rows.length > 0) return;
    await new Promise((resolve) => setTimeout(resolve, 20));
  }
  throw new Error(`nothing waited on ${event}`);
};

it("locks waiting servers in id order when rejecting, so a concurrent enroll does not deadlock", async () => {
  const [a, b] = await waiting("heap-first-big-id", "heap-second-small-id");
  if (a === undefined || b === undefined) throw new Error("two hosts");
  const big = "00000000-0000-4000-8000-0000000000f2";
  const small = "00000000-0000-4000-8000-0000000000f1";
  await harness.pool.query("update waiting_server set id = $1 where id = $2", [big, a.id]);
  await harness.pool.query("update waiting_server set id = $1 where id = $2", [small, b.id]);
  a.id = big;
  b.id = small;
  const gate = await harness.pool.connect();
  await gate.query("select pg_advisory_lock(934123)");
  await harness.pool.query(`
    create function pause_reject() returns trigger language plpgsql as $$ begin
      if new.id = '${big}' and new.rejected_at is not null and old.rejected_at is null then
        perform pg_advisory_xact_lock(934123);
      end if;
      return new;
    end $$;
    create trigger pause_reject before update on waiting_server for each row execute function pause_reject();
  `);
  try {
    const rejecting = failure(rejectWaitingServers(ada, { organizationSlug: "ada", ids: [a.id, b.id] }));
    await waitFor("advisory");
    const enrolling = failure(enroll([a, b], { first: b.id }));
    await waitFor("transactionid");
    await gate.query("select pg_advisory_unlock(934123)");
    expect(await rejecting).toBeNull();
    expect(await enrolling).toMatchObject({ _tag: "Validation", message: "The first server can't enroll: Not waiting." });
  } finally {
    await gate.query("select pg_advisory_unlock(934123)");
    gate.release();
    await harness.pool.query("drop trigger pause_reject on waiting_server; drop function pause_reject()");
  }
}, 15_000);

const lockWaits = async (count: number) => {
  for (let i = 0; i < 300; i++) {
    const { rows } = await harness.pool.query<{ n: number }>(
      "select count(*)::int as n from pg_stat_activity where datname = current_database() and wait_event_type = 'Lock'",
    );
    if ((rows[0]?.n ?? 0) >= count) return;
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
  throw new Error(`fewer than ${count} waited on a lock`);
};

it("ends a rejected host's open attempt while a probe selection waits on the same rows, without a deadlock", async () => {
  const [a, b] = await waiting("heap-first-big-id", "heap-second-small-id");
  if (a === undefined || b === undefined) throw new Error("two hosts");
  const big = "00000000-0000-4000-8000-0000000000f2";
  const small = "00000000-0000-4000-8000-0000000000f1";
  await harness.pool.query("update waiting_server set id = $1 where id = $2", [big, a.id]);
  await harness.pool.query("update waiting_server set id = $1 where id = $2", [small, b.id]);
  a.id = big;
  b.id = small;
  await run(enroll([b]));
  const blocker = await harness.pool.connect();
  let selecting: Promise<unknown> | undefined;
  let rejecting: Promise<unknown> | undefined;
  try {
    await blocker.query("begin");
    await blocker.query("select id from waiting_server where id = $1 for update", [small]);
    selecting = failure(openProbeSelection(ada, { organizationSlug: "ada", ids: [big, small] }));
    await lockWaits(1);
    rejecting = failure(rejectWaitingServers(ada, { organizationSlug: "ada", ids: [big, small] }));
    await lockWaits(2);
    await blocker.query("commit");
    expect(await Promise.all([selecting, rejecting])).toEqual([null, null]);
  } finally {
    await blocker.query("rollback");
    blocker.release();
    await selecting;
    await rejecting;
  }
  expect((await attempts()).map((row) => [row.state, row.ended_at !== null])).toEqual([["cancelled", true]]);
  expect(await outbox()).toContain("server/enrollment.cancel");
}, 15_000);

it("keeps a waiting server whose attempt an enroll committed while a Fleet Token revoke waited on it", async () => {
  const [a] = await waiting("web-a");
  if (a === undefined) throw new Error("one host");
  const [token] = (await harness.pool.query<{ id: string }>("select id from fleet_token")).rows;
  const gate = await harness.pool.connect();
  await gate.query("select pg_advisory_lock(934123)");
  await harness.pool.query(`
    create function pause_enroll() returns trigger language plpgsql as $$ begin
      perform pg_advisory_xact_lock(934123);
      return new;
    end $$;
    create trigger pause_enroll before insert on server_enrollment for each row execute function pause_enroll();
  `);
  try {
    const enrolling = run(enroll([a]));
    await waitFor("advisory");
    const revoking = run(revokeFleetToken(ada, { organizationSlug: "ada", id: token?.id ?? "" }));
    await waitFor("transactionid");
    await gate.query("select pg_advisory_unlock(934123)");
    const [started, revoked] = await Promise.all([enrolling, revoking]);
    expect(revoked).toEqual({ waitingRemoved: 0 });
    expect((await attempts()).map((row) => row.id)).toEqual(started.attempts.map((attempt) => attempt.id));
  } finally {
    await gate.query("select pg_advisory_unlock(934123)");
    gate.release();
    await harness.pool.query("drop trigger pause_enroll on server_enrollment; drop function pause_enroll()");
  }
}, 15_000);

it("locks attempts in id order when the stale sweep ends them, so a concurrent Forget does not deadlock", async () => {
  const [a, b] = await waiting("founder", "joiner");
  if (a === undefined || b === undefined) throw new Error("two hosts");
  const started = await run(enroll([a, b], { first: a.id }));
  expect(await claimAsFounder(a, "7".repeat(32))).toMatchObject({ kind: "initialize" });
  const seen = (await run(seeCluster(adaOrg))).seen;
  const big = "00000000-0000-4000-8000-0000000000f2";
  const small = "00000000-0000-4000-8000-0000000000f1";
  for (const [id, attempt] of [[big, started.attempts[0]?.id], [small, started.attempts[1]?.id]]) {
    await harness.pool.query("update server_enrollment set id = $1, requested_at = now() - interval '10 hours' where id = $2", [id, attempt]);
    await harness.pool.query("update machine_enrollment_token set server_enrollment_id = $1 where server_enrollment_id = $2", [id, attempt]);
  }
  const gate = await harness.pool.connect();
  await gate.query("select pg_advisory_lock(934124)");
  await harness.pool.query(`
    create function pause_stale() returns trigger language plpgsql as $$ begin
      if new.id = '${big}' and new.state = 'failed' then
        perform pg_advisory_xact_lock(934124);
      end if;
      return new;
    end $$;
    create trigger pause_stale before update on server_enrollment for each row execute function pause_stale();
  `);
  try {
    const sweeping = failure(closeStaleServerEnrollments());
    await waitFor("advisory");
    const forgetting = failure(forgetEmptiedPairing(adaOrg, seen.generation ?? ""));
    await waitFor("transactionid");
    await gate.query("select pg_advisory_unlock(934124)");
    expect(await Promise.all([sweeping, forgetting])).toEqual([null, null]);
  } finally {
    await gate.query("select pg_advisory_unlock(934124)");
    gate.release();
    await harness.pool.query("drop trigger pause_stale on server_enrollment; drop function pause_stale()");
  }
}, 15_000);

it("keeps an attempt an enroll committed while an expired-token check-in waited on it", async () => {
  const [a] = await waiting("expiry-race");
  if (a === undefined) throw new Error("one host");
  await harness.pool.query("update fleet_token set expires_at = clock_timestamp() + interval '1 second'");
  const gate = await harness.pool.connect();
  await gate.query("select pg_advisory_lock(934125)");
  await harness.pool.query(`
    create function pause_expiring_enroll() returns trigger language plpgsql as $$ begin
      perform pg_advisory_xact_lock(934125);
      return new;
    end $$;
    create trigger pause_expiring_enroll before insert on server_enrollment for each row execute function pause_expiring_enroll();
  `);
  try {
    const enrolling = run(enroll([a]));
    await waitFor("advisory");
    for (let i = 0; i < 100; i++) {
      const { rows } = await harness.pool.query<{ expired: boolean }>("select expires_at < clock_timestamp() as expired from fleet_token");
      if (rows[0]?.expired === true) break;
      await new Promise((resolve) => setTimeout(resolve, 20));
    }
    const checking = checkin(a);
    await waitFor("transactionid");
    await gate.query("select pg_advisory_unlock(934125)");
    const [started, answer] = await Promise.all([enrolling, checking]);
    expect((await attempts()).map((row) => row.id)).toEqual(started.attempts.map((attempt) => attempt.id));
    expect(answer.kind).toBe("enroll");
  } finally {
    await gate.query("select pg_advisory_unlock(934125)");
    gate.release();
    await harness.pool.query("drop trigger pause_expiring_enroll on server_enrollment; drop function pause_expiring_enroll()");
  }
}, 15_000);

it("holds Enroll behind Forget Servers before the first server founds, so no attempt keeps a deleted token", async () => {
  const [a, b] = await waiting("first-unclaimed", "second-unclaimed");
  if (a === undefined || b === undefined) throw new Error("two hosts");
  await run(enroll([a]));
  const gate = await harness.pool.connect();
  await gate.query("select pg_advisory_lock(934128)");
  await harness.pool.query(`
    create function pause_forget() returns trigger language plpgsql as $$ begin
      perform pg_advisory_xact_lock(934128);
      return new;
    end $$;
    create trigger pause_forget before update on machine_enrollment_token for each row execute function pause_forget();
  `);
  const store = await harness.runEffect(openCloudStore(harness.databaseUrl, harness.database, "test-encryption-secret"));
  try {
    const forgetting = run(forgetServers({ userId: ada.userId, organizationId: adaOrg }).pipe(
      Effect.provideService(CloudStore, { open: Effect.succeed(store) }),
    ));
    await waitFor("advisory");
    let settled = false;
    const enrolling = failure(enroll([b])).finally(() => { settled = true; });
    for (let i = 0; i < 100 && !settled; i++) {
      const { rows } = await harness.pool.query("select 1 from pg_stat_activity where datname = current_database() and wait_event = 'advisory'");
      if (rows.length >= 2) break;
      await new Promise((resolve) => setTimeout(resolve, 20));
    }
    await gate.query("select pg_advisory_unlock(934128)");
    expect(await forgetting).toMatchObject({ ok: true });
    expect(await enrolling).toBeNull();
    const open = (await harness.pool.query<{ host: string; role: string; token: string | null }>(`
      select se.waiting_server_id as host, se.role, t.id as token from server_enrollment se
        left join machine_enrollment_token t on t.id = se.enroll_token_id where se.ended_at is null
    `)).rows;
    expect(open).toEqual([{ host: b.id, role: "founder", token: expect.any(String) }]);
  } finally {
    await gate.query("select pg_advisory_unlock(934128)");
    gate.release();
    await harness.pool.query("drop trigger pause_forget on machine_enrollment_token; drop function pause_forget()");
  }
}, 15_000);

const removeUnpairedOrganizationWhile = async (close: (attemptId: string) => Effect.Effect<unknown, unknown, Needs>, bound: boolean) => {
  const [a] = await waiting("unclaimed-founder");
  if (a === undefined) throw new Error("one host");
  const attempt = (await run(enroll([a]))).attempts[0];
  if (attempt === undefined) throw new Error("one attempt");
  if (bound) await run(bindServerEnrollment({ attemptId: attempt.id, organizationId: adaOrg }, "org-removal-run"));
  const gate = await harness.pool.connect();
  await gate.query("select pg_advisory_lock(934129)");
  await harness.pool.query(`
    create function pause_token_delete() returns trigger language plpgsql as $$ begin
      perform pg_advisory_xact_lock(934129);
      return old;
    end $$;
    create trigger pause_token_delete after delete on machine_enrollment_token for each row execute function pause_token_delete();
  `);
  const store = await harness.runEffect(openCloudStore(harness.databaseUrl, harness.database, "test-encryption-secret"));
  try {
    const removing = failure(removeOrganization({ ...ada, organization: { id: adaOrg, slug: "ada" }, role: "owner", support: null }, "ada").pipe(
      Effect.provideService(CloudStore, { open: Effect.succeed(store) }),
    ));
    await waitFor("advisory");
    const closing = failure(close(attempt.id));
    await waitFor("transactionid");
    await gate.query("select pg_advisory_unlock(934129)");
    expect(await Promise.all([removing, closing])).toEqual([null, null]);
    expect((await harness.pool.query("select 1 from organization")).rows).toEqual([]);
    expect(await outbox()).toContain("server/enrollment.cancel");
  } finally {
    await gate.query("select pg_advisory_unlock(934129)");
    gate.release();
    await harness.pool.query("drop trigger pause_token_delete on machine_enrollment_token; drop function pause_token_delete()");
  }
};

it("ends an unpaired Organization's attempts before its tokens when removing it, so a run's cancellation does not deadlock", async () => {
  await removeUnpairedOrganizationWhile(() => closeServerEnrollment("org-removal-run", "cancellation"), true);
}, 15_000);

it("ends an unpaired Organization's attempts before its tokens when removing it, so a public Cancel does not deadlock", async () => {
  await removeUnpairedOrganizationWhile((id) => cancelServerEnrollment(ada, { organizationSlug: "ada", id }), false);
}, 15_000);

it("locks the attempt before its cancel event when cancelling, so an Organization removal does not deadlock", async () => {
  const [a] = await waiting("public-cancel-first");
  if (a === undefined) throw new Error("one host");
  const attempt = (await run(enroll([a]))).attempts[0];
  if (attempt === undefined) throw new Error("one attempt");
  const gate = await harness.pool.connect();
  await gate.query("select pg_advisory_lock(934137)");
  await harness.pool.query(`
    create function pause_cancel_event() returns trigger language plpgsql as $$ begin
      if new.name = 'server/enrollment.cancel' then perform pg_advisory_xact_lock(934137); end if;
      return new;
    end $$;
    create trigger pause_cancel_event after insert on operation_outbox for each row execute function pause_cancel_event();
  `);
  const store = await harness.runEffect(openCloudStore(harness.databaseUrl, harness.database, "test-encryption-secret"));
  try {
    const cancelling = failure(cancelServerEnrollment(ada, { organizationSlug: "ada", id: attempt.id }));
    await waitFor("advisory");
    const removing = failure(removeOrganization({ ...ada, organization: { id: adaOrg, slug: "ada" }, role: "owner", support: null }, "ada").pipe(
      Effect.provideService(CloudStore, { open: Effect.succeed(store) }),
    ));
    await waitFor("transactionid");
    await gate.query("select pg_advisory_unlock(934137)");
    expect(await Promise.all([cancelling, removing])).toEqual([null, null]);
    expect((await harness.pool.query("select 1 from organization")).rows).toEqual([]);
  } finally {
    await gate.query("select pg_advisory_unlock(934137)");
    gate.release();
    await harness.pool.query("drop trigger pause_cancel_event on operation_outbox; drop function pause_cancel_event()");
  }
}, 15_000);

it("holds Fleet Token creation behind an Organization removal", async () => {
  const gate = await harness.pool.connect();
  await gate.query("select pg_advisory_lock(934138)");
  await harness.pool.query(`
    create function pause_token_delete() returns trigger language plpgsql as $$ begin
      perform pg_advisory_xact_lock(934138);
      return old;
    end $$;
    create trigger pause_token_delete after delete on machine_enrollment_token for each row execute function pause_token_delete();
  `);
  await run(insertEnrollmentToken({ organizationId: adaOrg, userId: ada.userId }));
  const store = await harness.runEffect(openCloudStore(harness.databaseUrl, harness.database, "test-encryption-secret"));
  const advisoryWaiters = async () =>
    (await harness.pool.query("select 1 from pg_stat_activity where datname = current_database() and wait_event = 'advisory'")).rows.length;
  try {
    const removing = failure(removeOrganization({ ...ada, organization: { id: adaOrg, slug: "ada" }, role: "owner", support: null }, "ada").pipe(
      Effect.provideService(CloudStore, { open: Effect.succeed(store) }),
    ));
    await waitFor("advisory");
    let created = false;
    const creating = failure(createFleetToken(ada, { organizationSlug: "ada", name: "rack-2", expiresIn: "1d", autoEnroll: null }))
      .finally(() => { created = true; });
    for (let i = 0; i < 100 && !created && (await advisoryWaiters()) < 2; i++) await new Promise((resolve) => setTimeout(resolve, 20));
    expect(created).toBe(false);
    await gate.query("select pg_advisory_unlock(934138)");
    expect(await removing).toBeNull();
    expect(await creating).not.toBeNull();
    expect((await harness.pool.query("select 1 from fleet_token")).rows).toEqual([]);
  } finally {
    await gate.query("select pg_advisory_unlock(934138)");
    gate.release();
    await harness.pool.query("drop trigger pause_token_delete on machine_enrollment_token; drop function pause_token_delete()");
  }
}, 15_000);
