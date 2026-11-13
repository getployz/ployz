import type { BuildView, ConfigWritten, DeploymentView, PlanView, RemovalsView, RequestedServiceSpec, RuleView, Signed } from "@ployz/sdk";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { JsonValue } from "#/db/tables";
import { asTestDouble } from "#/lib/test-double";
import type { Review } from "#/modules/approval-session/approval-prompt";
import { canonicalJson, keyCode, signingInput, startSession, type ApprovalScope } from "#/modules/approval-session/approval-session";
import { approveAgain, approveEnvironmentDeploy, approveRedeploy, type Approving, type Follow, type Reads, type Servers } from "./deploy-approvals";

const CLUSTER = "CQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQk=";
const MACHINES = ["11111111111111111111111111111111", "22222222222222222222222222222222"];
const ENVIRONMENT = { id: "0f8fad5b-d9cb-469f-a165-70867728950e", project: "shop", name: "production", revision: 3 };
// The namespace of `PayloadDigest`'s golden fixture in ployz-core's authority/deploy_tests.rs.
const NAMESPACE = "app";
const scope: ApprovalScope = { environmentId: ENVIRONMENT.id, environmentName: "production", namespace: NAMESPACE, cluster: CLUSTER, machines: MACHINES };
const CAPABILITIES = ["ployz.container.deploy-approval.v1", "ployz.restart.session-grant.v2", "ployz.rule.commit-approval.v1"];

/** Every Server enforcing or not; the first runs `production`'s Namespace, and every other runs nothing. */
const placing = (enforced = true, runs: Record<string, readonly string[]> = { [NAMESPACE]: [MACHINES[0] ?? ""] }): Servers => ({
  running: (namespace) => runs[namespace] ?? [],
  machines: MACHINES.map((id, at) => ({ id, name: `server-${at}`, sealing: { kind: "held", public_key: CLUSTER }, deviceEnforcement: enforced, capabilities: CAPABILITIES })),
});

const SEALED = "QUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQQ==";
const web = (variable: string) => ({
  name: "web",
  mode: { mode: "replicated", replicas: 1 },
  container: {
    image: `nginx@sha256:${"0".repeat(63)}1`,
    pull_policy: "missing",
    environment: { GREETING: variable, SECRET: { sealed: SEALED, scope: "shared", key: "SECRET" } },
    sealed_identity: { organization: "org", environment: "11111111-1111-1111-1111-111111111111", service: "22222222-2222-2222-2222-222222222222" },
  },
});
// SAFETY: the fixture is a spec as the Store writes it on the wire, serde's defaults left out; ployz-core parses it.
const plan = (variable = "a  b", namespace = NAMESPACE): PlanView => ({
  environment: ENVIRONMENT, version: "v7", namespace, changes: [], unresolved: ["operations"],
  target: JSON.parse(JSON.stringify([web(variable)])) as RequestedServiceSpec[],
});

const COMMIT = "3f1c2a9b8e7d6c5b4a3f2e1d0c9b8a7f6e5d4c3b";
const RULE = "5e0c2b7a91d84f36a0e9c1b2d3f4a5b6";
const build = (over: Partial<BuildView> = {}): BuildView => ({
  service: "web", commit: COMMIT, status: "pending", message: null, started_at: null, finished_at: null, server: null, github_skipped: null, ...over,
});
// A Deployment with only the fields an approval reads.
const deployment = (builds: BuildView[] = [], inFlight = false) => asTestDouble<DeploymentView>()({
  id: "6a1d0f2e-1111-4222-8333-944455556666", environment: ENVIRONMENT, namespace: NAMESPACE, builds, in_flight: inFlight,
});
// A rule's body as the Store relays it; the browser reads only its ID and generation.
const rule = (body: string | null) => asTestDouble<RuleView>()({ environment: ENVIRONMENT, rule: body === null ? null : { body, signatures: [] } });

const reads = (over: Partial<Reads> = {}): Reads => ({
  plan: vi.fn(async () => plan()),
  namespace: vi.fn(async () => NAMESPACE),
  rule: vi.fn(async () => rule(JSON.stringify({ rule: RULE, generation: 2, extra: "kept by Cloud" }))),
  deployment: vi.fn(async () => deployment()),
  removals: vi.fn(async () => asTestDouble<RemovalsView>()({ environment: ENVIRONMENT, volumes: [] })),
  ...over,
});

const bytes = (text: string) => new TextEncoder().encode(text).buffer;
const raw = (text: string) => Uint8Array.from(text.match(/../g) ?? [], (pair) => parseInt(pair, 16));
const verifies = async (signed: Signed) => {
  const { key, signature } = signed.signatures[0] ?? expect.fail("unsigned");
  const publicKey = await crypto.subtle.importKey("raw", raw(key), { name: "Ed25519" }, false, ["verify"]);
  return crypto.subtle.verify({ name: "Ed25519" }, publicKey, raw(signature), signingInput(signed.body));
};
const review = vi.fn<(review: Review) => Promise<boolean>>();
const approving = (store: Reads, servers: Servers): Approving => ({ reads: store, servers, review });
const shown = () => review.mock.calls.at(-1)?.[0].lines ?? [];

beforeEach(async () => {
  vi.stubGlobal("location", { hostname: "localhost" });
  vi.stubGlobal("navigator", { credentials: { get: async () => ({
    rawId: bytes("credential"),
    response: { authenticatorData: bytes("data"), clientDataJSON: bytes("{}"), signature: bytes("signature") },
  }) } });
  await startSession(scope);
});
afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
  review.mockReset();
});

describe("approveRedeploy of an image", () => {
  it("reads the plan of the Deployment it ships again and signs exactly the body it showed, after the person approves", async () => {
    const sign = vi.spyOn(crypto.subtle, "sign");
    review.mockImplementation(async () => {
      expect(sign).not.toHaveBeenCalled();
      return true;
    });
    const store = reads();
    const id = "9b2f6e0a-aaaa-4bbb-8ccc-dddddddddddd";
    const approved = await approveRedeploy(approving(store, placing()), deployment(), id) ?? expect.fail("approved");
    expect(store.plan).toHaveBeenCalledWith({ project: "shop", environment: "production" }, deployment().id);
    const approval = approved.fields.approval ?? expect.fail("a deploy approval");
    expect(approved.fields.approvals).toBeUndefined();
    expect(approved.again).toBeNull();
    expect(await verifies(approval)).toBe(true);
    // SAFETY: the body is a canonical DeployApproval this browser drafted.
    const body = JSON.parse(approval.body) as { operation: string; issued_at: number; expires_at: number; signer: { browser: string } };
    // SAFETY: as above.
    expect(canonicalJson(JSON.parse(approval.body) as JsonValue)).toBe(approval.body);
    expect(body.operation).toBe(id.replaceAll("-", ""));
    // Every term it signs, read back from the bytes it signed, spacing kept.
    const lines = shown();
    expect(lines[0]).toMatch(/^Approved web: nginx@sha256:0{63}1/);
    expect(lines).toContain('  variables GREETING="a  b", SECRET (sealed)');
    expect(lines).toContain(`  deployment ${body.operation}`);
    expect(lines).toContain(`  namespace ${NAMESPACE}`);
    expect(lines).toContain(`  servers ${MACHINES.join(", ")}`);
    expect(lines).toContain(`  cluster "${CLUSTER}"`);
    expect(lines).toContain(`  signer browser key ${await keyCode(raw(body.signer.browser))}`);
    const utc = (at: number) => new Date(at).toISOString().replace(/\.\d{3}Z$/, "Z");
    expect(lines).toContain(`  valid ${utc(body.issued_at)} to ${utc(body.expires_at)}`);
  });

  it("signs nothing when the person cancels, and shows values that sign differently apart", async () => {
    const sign = vi.spyOn(crypto.subtle, "sign");
    review.mockResolvedValue(false);
    expect(await approveRedeploy(approving(reads({ plan: vi.fn(async () => plan("a b")) }), placing()), deployment(), crypto.randomUUID())).toBeNull();
    const one = shown();
    expect(await approveRedeploy(approving(reads(), placing()), deployment(), crypto.randomUUID())).toBeNull();
    expect(sign).not.toHaveBeenCalled();
    expect(one).toContain('  variables GREETING="a b", SECRET (sealed)');
    expect(shown()).toContain('  variables GREETING="a  b", SECRET (sealed)');
  });

  it("refuses a plan of another Environment before it shows or signs anything", async () => {
    const sign = vi.spyOn(crypto.subtle, "sign");
    await expect(approveRedeploy(approving(reads({ plan: vi.fn(async () => plan("a  b", "staging")) }), placing()), deployment(), crypto.randomUUID()))
      .rejects.toThrow("The Environment changed after this review.");
    expect(review).not.toHaveBeenCalled();
    expect(sign).not.toHaveBeenCalled();
  });

  it("asks nothing and reads nothing where no Server enforces, so the redeploy goes as before", async () => {
    const store = reads();
    expect(await approveRedeploy(approving(store, placing(false)), deployment(), crypto.randomUUID())).toBeUndefined();
    expect(store.plan).not.toHaveBeenCalled();
    expect(review).not.toHaveBeenCalled();
  });
});

describe("approveRedeploy of a GitHub build", () => {
  it("approves each commit under the rule Cloud relays, shows every term, and waits to approve again", async () => {
    review.mockResolvedValue(true);
    const approved = await approveRedeploy(approving(reads(), placing()), deployment([build()]), crypto.randomUUID()) ?? expect.fail("approved");
    expect(approved.fields.awaits_approval).toBe(true);
    expect(approved.fields.approval).toBeUndefined();
    const [approval] = approved.fields.approvals ?? [];
    if (approval === undefined) return expect.fail("a commit approval");
    expect(await verifies(approval)).toBe(true);
    const body = JSON.parse(approval.body) as { issued_at: number; signer: { browser: string } };
    // ployz-core's `CommitApproval`, every field and no other, in canonical form.
    expect(approval.body).toBe(canonicalJson({
      cluster: CLUSTER, commit: COMMIT, expires_at: body.issued_at + 15 * 60_000, generation: 2, issued_at: body.issued_at,
      rule: RULE, service: "web", signer: { browser: body.signer.browser },
    }));
    expect(shown().slice(0, 4)).toEqual([
      "Approving commits for this redeploy:",
      `  web at ${COMMIT}`,
      `  rule ${RULE}, generation 2`,
      `  cluster "${CLUSTER}"`,
    ]);
    expect(approved.again?.terms).toEqual({ rule: RULE, generation: 2, commits: [{ service: "web", commit: COMMIT }] });
  });

  it("refuses an upload or an Environment without a rule before it signs", async () => {
    const sign = vi.spyOn(crypto.subtle, "sign");
    review.mockResolvedValue(true);
    await expect(approveRedeploy(approving(reads(), placing()), deployment([build({ commit: null })]), crypto.randomUUID()))
      .rejects.toThrow("web builds from an upload, which these servers won't run.");
    await expect(approveRedeploy(approving(reads({ rule: vi.fn(async () => rule(null)) }), placing()), deployment([build()]), crypto.randomUUID()))
      .rejects.toThrow("production has no automatic deploy rule.");
    expect(sign).not.toHaveBeenCalled();
  });
});

describe("approveAgain", () => {
  const again = { scope, terms: { rule: RULE, generation: 2, commits: [{ service: "web", commit: COMMIT }] } };
  const follow = (views: DeploymentView[]): Follow & { approved: Signed[][]; cancelled: number } => {
    const state = { approved: [] as Signed[][], cancelled: 0 };
    return Object.assign(state, {
      read: async () => views.shift() ?? expect.fail("read past the end"),
      // SAFETY: the loop only waits on the Store's answer, never reads it.
      approve: async (approvals: Signed[]) => (state.approved.push(approvals), {} as ConfigWritten),
      // SAFETY: as above.
      cancel: async () => (state.cancelled += 1, {} as ConfigWritten),
      wait: async () => undefined,
    });
  };

  it("approves the same commits again with new times once every build ended, without asking", async () => {
    const run = follow([deployment([build({ status: "building" })], true), deployment([build({ status: "reused" })], true)]);
    await approveAgain(run, again);
    expect(review).not.toHaveBeenCalled();
    expect(run.cancelled).toBe(0);
    const [approval] = run.approved[0] ?? [];
    if (approval === undefined) return expect.fail("approved again");
    expect(await verifies(approval)).toBe(true);
    expect(JSON.parse(approval.body)).toMatchObject({ commit: COMMIT, rule: RULE, generation: 2, service: "web" });
  });

  it("cancels the redeploy and signs nothing when a build fails or the redeploy ended", async () => {
    const failed = follow([deployment([build({ status: "failed", message: "exit 1" })], true)]);
    await expect(approveAgain(failed, again)).rejects.toThrow(`GitHub didn't build web at ${COMMIT.slice(0, 12)}: exit 1.`);
    const ended = follow([deployment([build()], false)]);
    await expect(approveAgain(ended, again)).rejects.toThrow("The redeploy ended before this browser approved it.");
    expect([failed.cancelled, ended.cancelled]).toEqual([1, 1]);
    expect([...failed.approved, ...ended.approved]).toEqual([]);
  });
});

describe("approveEnvironmentDeploy", () => {
  const branch = { project: "shop", environment: "fix-web" };
  const parent = { project: "shop", environment: "production" };

  it("signs the plan it read now and sends its version, so the Store refuses it once the Environment changed", async () => {
    review.mockResolvedValue(true);
    const store = reads();
    const id = crypto.randomUUID();
    const approved = await approveEnvironmentDeploy(approving(store, placing()), parent, id, parent) ?? expect.fail("approved");
    expect(store.plan).toHaveBeenCalledWith(parent);
    expect(store.namespace).not.toHaveBeenCalled();
    expect(approved.version).toBe("v7");
    expect(JSON.parse(approved.approval.body)).toMatchObject({ operation: id.replaceAll("-", ""), namespace: NAMESPACE });
    expect(await verifies(approved.approval)).toBe(true);
  });

  it("refuses a plan Cloud sends of another Environment than the one pressed, before it asks or signs", async () => {
    review.mockResolvedValue(true);
    const store = reads({ plan: vi.fn(async () => plan()) });
    await expect(approveEnvironmentDeploy(approving(store, placing()), branch, crypto.randomUUID(), parent))
      .rejects.toThrow("Cloud sent the plan of another environment.");
    const other = { ...plan(), environment: { ...ENVIRONMENT, id: crypto.randomUUID() } };
    await expect(approveRedeploy(approving(reads({ plan: vi.fn(async () => other) }), placing()), deployment(), crypto.randomUUID()))
      .rejects.toThrow("Cloud sent the plan of another environment.");
    expect(review).not.toHaveBeenCalled();
  });

  it("asks nothing for a plan that builds from source, so the Deploy goes as before and runs under its rule", async () => {
    review.mockResolvedValue(true);
    const built = { ...plan(), environment: { ...ENVIRONMENT, name: "fix-web" } };
    const [spec] = built.target ?? [];
    if (spec === undefined) return expect.fail("a spec");
    spec.container.image = "ployz-build/web:pending";
    const store = reads({ plan: vi.fn(async () => built) });
    const prompted = vi.spyOn(navigator.credentials, "get");
    expect(await approveEnvironmentDeploy(approving(store, placing()), branch, crypto.randomUUID(), parent)).toBeUndefined();
    expect(prompted).not.toHaveBeenCalled();
    expect(review).not.toHaveBeenCalled();
  });

  it("names the Cluster its Parent runs on for a Branch that runs nowhere yet, and refuses when Cloud can't tell which", async () => {
    review.mockResolvedValue(true);
    const store = reads({ plan: vi.fn(async () => ({ ...plan(), environment: { ...ENVIRONMENT, name: "fix-web" } })), namespace: vi.fn(async () => "production-ns") });
    const servers = placing(true, { "production-ns": [MACHINES[1] ?? ""] });
    // Two Clusters, so only the Parent's Servers say which one the Branch deploys to.
    const other = { ...servers, machines: [...servers.machines, { id: "3".repeat(32), name: "elsewhere", sealing: { kind: "held", public_key: "b".repeat(43) + "=" }, deviceEnforcement: true, capabilities: CAPABILITIES }] };
    const approved = await approveEnvironmentDeploy(approving(store, other), branch, crypto.randomUUID(), parent) ?? expect.fail("approved");
    expect(store.namespace).toHaveBeenCalledWith(parent);
    expect(JSON.parse(approved.approval.body)).toMatchObject({ cluster: CLUSTER, machines: MACHINES });
    const nowhere = reads({ plan: store.plan, namespace: vi.fn(async () => "nowhere") });
    await expect(approveEnvironmentDeploy(approving(nowhere, other), branch, crypto.randomUUID(), parent)).rejects.toThrow("Cloud couldn't check the servers.");
  });
});
