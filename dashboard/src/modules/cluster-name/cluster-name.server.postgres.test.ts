import { eq } from "drizzle-orm";
import { Effect } from "effect";
import { Inngest } from "inngest";
import type { JsonValue } from "@ployz/sdk";
import { afterAll, beforeAll, beforeEach, expect, it, vi } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { operationApprovals } from "#/modules/approvals/tables";
import { clusterRename } from "#/modules/cluster-name/cluster-name.commands.server";
import { applyClusterName, foundClusterName, freshRenameDigest, listUnappliedClusterNames, renameCluster } from "#/modules/cluster-name/cluster-name.server";
import { organizationClusterName } from "#/modules/cluster-name/tables";
import type { Caller } from "#/modules/identity/actor";
import { InngestClient } from "#/modules/inngest/client";
import { ServerLacks } from "#/modules/runtime/daemon-compat";
import { OrganizationRuntime, type ScopedRuntimeClientSession } from "#/modules/runtime/organization-runtime.server";
import type { PloyzSdkError, PloyzSession } from "#/modules/runtime/ployz.server";
import type { Database } from "#/server/database.server";
import { type PostgresTestHarness, startPostgresTestHarness } from "#/test/postgres";

const ORG = "00000000-0000-4000-8000-00000000c1a1";
const ADA = "00000000-0000-4000-8000-00000000c1a2";
const SLUG = "a".repeat(70);

let harness: PostgresTestHarness;
const inngest = new Inngest({ id: "cluster-name-test" });
vi.spyOn(inngest, "send").mockResolvedValue({ ids: [] });

let session: ScopedRuntimeClientSession = { status: "no_connection" };
const named: string[] = [];
const connected = (answer: (name: string) => Effect.Effect<string | null, PloyzSdkError>): ScopedRuntimeClientSession => ({
  status: "connected",
  connected: asTestDouble<PloyzSession>()({ setClusterName: (name: string) => { named.push(name); return answer(name); } }),
});
const runtime = asTestDouble<OrganizationRuntime["Service"]>()({ cancel: () => Effect.void, open: () => Effect.sync(() => session) });

const run = <A, E>(effect: Effect.Effect<A, E, Database | InngestClient | OrganizationRuntime>) =>
  harness.runEffect(effect.pipe(Effect.provideService(InngestClient, inngest), Effect.provideService(OrganizationRuntime, runtime)));

const as = (via: Caller["via"], input: JsonValue, who: Pick<Caller, "role" | "support"> = { role: "owner", support: null }) => run(clusterRename.invoke({
  userId: ADA,
  organization: { id: ORG, slug: SLUG },
  credential: { kind: "session", id: "ada-session" },
  ...who,
  via,
}, input, { approvalId: null, key: null }));

const row = async () => (await harness.db.select().from(organizationClusterName).where(eq(organizationClusterName.organizationId, ORG)))[0];
const approvals = async () => (await harness.db.select({ id: operationApprovals.id }).from(operationApprovals)).length;

beforeAll(async () => {
  harness = await startPostgresTestHarness();
}, 60_000);

afterAll(async () => {
  await harness.stop();
});

beforeEach(async () => {
  named.length = 0;
  session = { status: "no_connection" };
  await harness.pool.query(`
    truncate table operation_outbox, operation_approvals, organization_pairing, "user", organization cascade;
    insert into organization (id, name, slug) values ('${ORG}', 'Acme', '${SLUG}');
    insert into "user" (id, email, name) values ('${ADA}', 'ada@example.test', 'Ada');
    insert into member (id, organization_id, user_id, role, created_at) values (gen_random_uuid(), '${ORG}', '${ADA}', 'owner', now());
  `);
});

const founded = () => harness.pool.query(
  "insert into organization_pairing (organization_id, encrypted_pairing_secret, founder_claim_machine_id, founder_machine_id) values ($1, $2, $3, $3)",
  [ORG, JSON.stringify({ v: 1 }), "1".repeat(32)],
);

it("renames from the dashboard at once, and asks a human first from the CLI or an agent", async () => {
  expect(await as("dashboard", { name: "acme" })).toMatchObject({ ok: true, output: { name: "acme" } });
  expect(await row()).toMatchObject({ name: "acme", appliedName: null, refusal: null });
  expect(await approvals()).toBe(0);

  for (const via of ["agent", "cli"] as const) {
    expect(await as(via, { name: "beta" })).toMatchObject({ ok: false, refusal: { code: "approval_required", message: "A human must approve renaming the cluster to beta before it runs.", writes: "none" } });
  }
  expect(await approvals()).toBe(1);
  expect((await row())?.name).toBe("acme");
  const [asked] = await harness.db.select({ review: operationApprovals.review, subject: operationApprovals.subject }).from(operationApprovals);
  expect(asked).toMatchObject({ subject: `cluster:${ORG}`, review: { effects: [], operation: { verb: "set_cluster_name", name: "beta", preview: { from: "acme", to: "beta" } } } });
});

it("never lets a support visit rename the Cluster", async () => {
  const support = { role: null, support: { variables: true, configs: true, logs: true } };
  expect(await as("dashboard", { name: "by-support" }, support)).toMatchObject({ ok: false, refusal: { code: "forbidden" } });
  expect(await row()).toBeUndefined();
});

it("supersedes a pending rename once the name moves, so its card never shows a stale name", async () => {
  await as("dashboard", { name: "acme" });
  await as("cli", { name: "beta" });
  const [asked] = await harness.db.select({ digest: operationApprovals.digest, review: operationApprovals.review }).from(operationApprovals);
  const review = asked?.review;
  if (asked === undefined || review === undefined || !("operation" in review)) throw new Error("an operation approval");
  const fresh = () => run(freshRenameDigest(ORG, asked.digest, review.operation.preview));
  expect(await fresh()).toBe(asked.digest);
  await as("dashboard", { name: "gamma" });
  expect(await fresh()).not.toBe(asked.digest);
  await as("dashboard", { name: "beta" });
  expect(await fresh()).toBeNull();
});

it("refuses a name that isn't one DNS label, naming the rule", async () => {
  expect(await as("dashboard", { name: "a".repeat(64) })).toMatchObject({ ok: false, refusal: { code: "invalid_argument", message: "63 characters at most." } });
  expect(await as("dashboard", { name: "Acme Prod" })).toMatchObject({
    ok: false,
    refusal: { code: "invalid_argument", message: "Use lowercase letters, numbers and hyphens. Start and end with a letter or number." },
  });
  expect(await row()).toBeUndefined();
});

it("founds with the slug cut to 63 characters, keeping a name saved earlier", async () => {
  await run(foundClusterName(ORG));
  expect(await row()).toMatchObject({ name: "a".repeat(63), appliedName: null });
  await harness.pool.query("update organization_cluster_name set name = 'acme', applied_name = 'acme' where organization_id = $1", [ORG]);
  await run(foundClusterName(ORG));
  expect(await row()).toMatchObject({ name: "acme", appliedName: null });
});

it("waits until a Server answers, then records the name it confirmed", async () => {
  await as("dashboard", { name: "acme" });
  expect(await run(applyClusterName(ORG))).toBe("unfounded");
  await founded();
  session = { status: "unreachable", error: null };
  expect(await run(applyClusterName(ORG))).toBe("waiting");
  expect(await run(listUnappliedClusterNames())).toEqual([ORG]);

  session = connected((name) => Effect.succeed(name));
  expect(await run(applyClusterName(ORG))).toBe("current");
  expect(named).toEqual(["acme"]);
  expect(await row()).toMatchObject({ name: "acme", appliedName: "acme", refusal: null });
  expect(await run(listUnappliedClusterNames())).toEqual([]);
  expect(await run(applyClusterName(ORG))).toBe("current");
  expect(named).toEqual(["acme"]);
});

it("keeps a name saved while a set was in flight pending", async () => {
  await founded();
  await as("dashboard", { name: "acme" });
  session = connected((name) => Effect.promise(() => harness.pool.query("update organization_cluster_name set name = 'beta'")).pipe(Effect.as(name)));
  expect(await run(applyClusterName(ORG))).toBe("waiting");
  expect(await row()).toMatchObject({ name: "beta", appliedName: null });
});

it("sends a name renamed back to while another was in flight, never trusting the old confirmation", async () => {
  await founded();
  await as("dashboard", { name: "acme" });
  session = connected((name) => Effect.succeed(name));
  expect(await run(applyClusterName(ORG))).toBe("current");
  await as("dashboard", { name: "beta" });
  session = connected((name) => Effect.promise(() => run(renameCluster(ORG, "acme"))).pipe(Effect.as(name)));
  expect(await run(applyClusterName(ORG))).toBe("waiting");
  session = connected((name) => Effect.succeed(name));
  expect(await run(applyClusterName(ORG))).toBe("current");
  expect(named).toEqual(["acme", "beta", "acme"]);
  expect(await row()).toMatchObject({ name: "acme", appliedName: "acme" });
});

it("shows why a Server too old for Cluster Names refused, and tries again later", async () => {
  await founded();
  await as("dashboard", { name: "acme" });
  session = connected(() => Effect.fail(new ServerLacks({ machineId: "1".repeat(32), capability: "ployz.cluster.name.v1" })));
  expect(await run(applyClusterName(ORG))).toBe("refused");
  expect(await row()).toMatchObject({ refusal: "Needs a newer Ployz on your servers.", appliedName: null });
  expect(await run(listUnappliedClusterNames())).toEqual([ORG]);
  await as("dashboard", { name: "beta" });
  expect(await row()).toMatchObject({ name: "beta", refusal: null });
});
