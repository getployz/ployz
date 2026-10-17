import { generateKeyPairSync, type KeyObject, sign } from "node:crypto";
import { Cause, Effect, Exit } from "effect";
import { Inngest } from "inngest";
import { afterAll, beforeAll, beforeEach, expect, it, vi } from "vitest";
import { InngestClient } from "#/modules/inngest/client";
import { serverEnrollmentReportedEvent } from "#/modules/inngest/events";
import { hashEnrollmentToken } from "#/modules/machines/enrollment.server";
import type { CheckinRequest, HostReport, Prep } from "#/modules/machines/waiting/checkin-wire";
import { checkIn, checkinMessage } from "#/modules/machines/waiting/checkin.server";
import {
  cancelServerEnrollment,
  readServerEnrollments,
  startServerEnrollment,
} from "#/modules/machines/waiting/enroll.server";
import { closeServerEnrollment, releaseJoiner } from "#/modules/machines/waiting/enrollment-run.server";
import { createFleetToken, revokeFleetToken } from "#/modules/machines/waiting/fleet-token.server";
import { rejectWaitingServers } from "#/modules/machines/waiting/reject.server";
import { type EnrollInput, ZFS_BUILD_FAILED } from "#/modules/machines/waiting/server-enrollment";
import type { Database } from "#/server/database.server";
import type { SecretEncryption } from "#/utils/encrypted-secret.server";
import { type PostgresTestHarness, startPostgresTestHarness } from "#/test/postgres";

const ada = { userId: "00000000-0000-4000-8000-000000000801" };
const adaOrg = "00000000-0000-4000-8000-000000000811";

let harness: PostgresTestHarness;
const inngest = new Inngest({ id: "enroll-test" });
const sent = vi.spyOn(inngest, "send").mockResolvedValue({ ids: [] });

const run = <A, E>(operation: Effect.Effect<A, E, Database | SecretEncryption | InngestClient>) =>
  harness.runEffect(Effect.provideService(operation, InngestClient, inngest));

const failure = async <A, E>(operation: Effect.Effect<A, E, Database | SecretEncryption | InngestClient>) => {
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
  await harness.pool.query("insert into organization_pairing (organization_id, encrypted_pairing_secret, founder_public_key, founder_claim_machine_id) values ($1, '{}'::jsonb, 'k', $2)", [adaOrg, "00000000000000000000000000000001"]);
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

it("plans a first server when the Organization's pairing was claimed but never founded and no first server is on its way", async () => {
  const [a, b] = await waiting("web-a", "web-b");
  if (a === undefined || b === undefined) throw new Error("two hosts");
  await harness.pool.query("insert into organization_pairing (organization_id, encrypted_pairing_secret, founder_public_key, founder_claim_machine_id) values ($1, '{}'::jsonb, 'k', $2)", [adaOrg, "00000000000000000000000000000001"]);
  const started = await run(enroll([a, b], { first: a.id }));
  expect(started.attempts.map((attempt) => [attempt.waitingServerId, attempt.role, attempt.state])).toEqual([
    [a.id, "founder", "instructed"],
    [b.id, "joiner", "awaiting_founder"],
  ]);
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
