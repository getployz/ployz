import type { RequestedServiceSpec } from "@ployz/sdk";
import type { JsonValue } from "#/db/tables";
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  actScope,
  approveAct,
  approveDeploy,
  approveProposal,
  type ApprovalScope,
  type ApprovalSession,
  canonicalJson,
  type DeployPlan,
  deployScope,
  grantBody,
  grantChallenge,
  hex,
  keyCode,
  promptReason,
  readSession,
  signingInput,
  startSession,
  stepDigest,
} from "./approval-session";
import { actApprovals, deployApprovalScope, ensureApproval, proposalApprovals } from "./approval-prompt";

const production: ApprovalScope = {
  environmentId: "0f8fad5b-d9cb-469f-a165-70867728950e",
  environmentName: "production",
  namespace: "production",
  cluster: "CQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQk=",
  machines: ["22222222222222222222222222222222", "11111111111111111111111111111111"],
};

const session = (scope: ApprovalScope, expiresAt: number): ApprovalSession =>
  ({ scope, families: ["deploy", "restart", "start", "stop"], issuedAt: expiresAt - 3_600_000, expiresAt, keyCode: "abcdefghij" });

describe("canonicalJson", () => {
  // The fixtures of `ployz_core::authority::canonical`'s tests.
  it("sorts keys and drops whitespace", () => {
    expect(canonicalJson({ b: 1, a: [true, "x"], A: { z: 0 } })).toBe(`{"A":{"z":0},"a":[true,"x"],"b":1}`);
  });

  it("escapes strings as JSON.stringify does", () => {
    expect(canonicalJson("q\"\\\b\t\n\f\r\u0001\u001f/é ")).toBe("\"q\\\"\\\\\\b\\t\\n\\f\\r\\u0001\\u001f/é \"");
  });

  it("refuses values JavaScript cannot hold exactly, and null", () => {
    expect(() => canonicalJson(1.5)).toThrow();
    expect(() => canonicalJson(-1)).toThrow();
    expect(() => canonicalJson(2 ** 53)).toThrow();
    expect(() => canonicalJson(null)).toThrow();
    expect(() => canonicalJson({ é: 1 })).toThrow();
    expect(canonicalJson({ a: false, b: [] })).toBe(`{"a":false,"b":[]}`);
  });
});

describe("grantBody", () => {
  // `ployz_core::authority::session::tests::the_browser_writes_this_grant_body_and_challenge`.
  it("writes the bytes and challenge a Server checks", async () => {
    const browserKey = "ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c";
    const body = grantBody(production, browserKey, 1_000_000);
    expect(body).toBe(
      `{"browser_key":"${browserKey}","cluster":"${production.cluster}","delegation":"none","environment":"production",`
        + `"environment_id":"${production.environmentId}","expires_at":4600000,`
        + `"families":["deploy","restart","start","stop"],"issued_at":1000000,"machines":["11111111111111111111111111111111","22222222222222222222222222222222"]}`,
    );
    const rustBody = body.replace(`"families":["deploy","restart","start","stop"]`, `"families":["restart","stop"]`);
    expect(hex(await grantChallenge(rustBody))).toBe("e02a7c52d5c358134a9af54f1b72df1c0fb5b02a89fefb1be3f260c5fcde15df");
  });
});

describe("keyCode", () => {
  it("follows the Device code rule, matching `DeviceCode` for the Rust fixture key", async () => {
    const key = Uint8Array.from("ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c".match(/../g) ?? [], (pair) => parseInt(pair, 16));
    expect(await keyCode(key)).toBe("72asyextvn");
  });
});

describe("promptReason", () => {
  const now = 10_000_000;
  it("prompts first, again once expired, and for an Environment the session doesn't cover", () => {
    expect(promptReason(null, production, "restart", now)).toBe("first");
    expect(promptReason(session(production, now - 1), production, "restart", now)).toBe("expired");
    const fixApi = { ...production, environmentId: "env-fix-api", environmentName: "fix-api", namespace: "fix-api" };
    expect(promptReason(session(fixApi, now + 1_800_000), production, "restart", now)).toBe("scope");
    expect(promptReason(session(production, now + 1_800_000), production, "restart", now)).toBeNull();
  });

  it("prompts for a Server or an action the grant doesn't name", () => {
    const live = session(production, now + 1_800_000);
    expect(promptReason(live, { ...production, machines: ["33333333333333333333333333333333"] }, "restart", now)).toBe("scope");
    // The same Environment ID under another Namespace is another Environment to the Machine, so it prompts.
    expect(promptReason(live, { ...production, namespace: "payments-prod" }, "restart", now)).toBe("scope");
    expect(promptReason({ ...live, families: ["restart"] }, production, "stop", now)).toBe("scope");
    expect(promptReason(live, production, "stop", now)).toBeNull();
    expect(promptReason(live, production, "start", now)).toBeNull();
  });

  it("prompts again once less than one approval's 15 minutes is left, so no approval is cut short", () => {
    expect(promptReason(session(production, now + 15 * 60_000 - 1), production, "restart", now)).toBe("expired");
    expect(promptReason(session(production, now + 15 * 60_000), production, "restart", now)).toBeNull();
  });
});

describe("actScope", () => {
  const held = { kind: "held", public_key: production.cluster };
  const environment = { environmentId: "env-production", environmentName: "production", namespace: "production" };

  it("names the copies' Cluster and every Server in it, once a host enforces, so the session covers a deploy too", () => {
    const result = actScope(environment, [{ machineId: "22222222222222222222222222222222" }], [
      { id: "22222222222222222222222222222222", sealing: held, deviceEnforcement: true },
      { id: "11111111111111111111111111111111", sealing: held },
      { id: "33333333333333333333333333333333", sealing: { kind: "held", public_key: "other" } },
      { id: "44444444444444444444444444444444", sealing: held },
    ]);
    expect(result).toEqual({
      enforced: true,
      scope: { ...environment, cluster: production.cluster, machines: ["11111111111111111111111111111111", "22222222222222222222222222222222", "44444444444444444444444444444444"] },
    });
  });

  it("asks nothing of Servers that don't enforce", () => {
    expect(actScope(environment, [{ machineId: "m" }], [{ id: "m", sealing: held, deviceEnforcement: false }]).enforced).toBe(false);
  });
});

describe("signingInput", () => {
  it("prefixes the body as `keys::signing_input` does for a workflow", () => {
    expect(new TextDecoder().decode(signingInput("{}"))).toBe("ployz-authority/v1\nworkflow\n{}");
  });
});

describe("approveAct", () => {
  afterEach(() => void vi.unstubAllGlobals());
  const copy = { machine: "22222222222222222222222222222222", container: "c".repeat(64), hook: false };
  const hook = { machine: "22222222222222222222222222222222", container: "d".repeat(64), hook: true };
  const bytes = (text: string) => new TextEncoder().encode(text).buffer;
  const passkey = async (now: number) => {
    vi.stubGlobal("location", { hostname: "localhost" });
    vi.stubGlobal("navigator", { credentials: { get: async () => ({
      rawId: bytes("credential"),
      response: { authenticatorData: bytes("data"), clientDataJSON: bytes("{}"), signature: bytes("signature") },
    }) } });
    await startSession(production, now);
  };

  it("signs a stop and a start of each copy under the one session, naming the press's kind and Server", async () => {
    const now = Date.now();
    await passkey(now);
    for (const verb of ["restart", "stop", "start"] as const) {
      const approvals = await approveAct(verb, production, "web", [copy], now);
      const signed = approvals[copy.container] ?? expect.fail("no approval");
      expect(JSON.parse(signed.body)).toMatchObject({ kind: verb, namespace: "production", machines: [copy.machine], container: copy.container });
      expect(JSON.parse(signed.grants?.[0]?.body ?? "{}")).toMatchObject({ families: ["deploy", "restart", "start", "stop"], environment: "production" });
    }
  });

  it("never lets a hook's container start: a start skips it, a restart or stop gives it a stop approval", async () => {
    const now = Date.now();
    await passkey(now);
    expect(Object.keys(await approveAct("start", production, "web", [copy, hook], now))).toEqual([copy.container]);
    for (const verb of ["restart", "stop"] as const) {
      const approvals = await approveAct(verb, production, "web", [copy, hook], now);
      expect(JSON.parse(approvals[copy.container]?.body ?? "{}")).toMatchObject({ kind: verb });
      expect(JSON.parse(approvals[hook.container]?.body ?? "{}")).toMatchObject({ kind: "stop", container: hook.container });
    }
  });

  it("refuses a stop a Server couldn't start again before it prompts, in an Environment that would prompt", async () => {
    const get = vi.fn();
    vi.stubGlobal("location", { hostname: "localhost" });
    vi.stubGlobal("navigator", { credentials: { get } });
    const environment = { environmentId: "env-fix-api", environmentName: "fix-api", namespace: "fix-api" };
    const held = { kind: "held", public_key: production.cluster } as const;
    const target = {
      environment,
      runtime: "web",
      copies: [{ id: copy.container, machineId: copy.machine, hook: false }],
      machines: [{ id: copy.machine, name: "machine-2", sealing: held, deviceEnforcement: true, capabilities: ["ployz.restart.session-grant.v2"] }],
    };
    const { scope } = actScope(environment, target.copies, target.machines);
    expect(promptReason(readSession(), scope ?? expect.fail("no scope"), "stop")).not.toBeNull();
    await expect(actApprovals("stop", target)).rejects.toThrow("machine-2 needs a newer Ployz to stop it.");
    expect(get).not.toHaveBeenCalled();
  });

  it("signs nothing for a scope the session doesn't cover, so it never approves another Environment, Cluster or Server", async () => {
    const now = Date.now();
    await passkey(now);
    const others = [
      { ...production, environmentId: "env-fix-api", namespace: "fix-api" },
      { ...production, cluster: "other" },
      { ...production, machines: [...production.machines, "33333333333333333333333333333333"] },
    ];
    for (const scope of others) await expect(approveAct("stop", scope, "web", [copy], now)).rejects.toThrow("Confirm it's you again.");
    await expect(approveAct("stop", production, "web", [{ ...copy, machine: "33333333333333333333333333333333" }], now)).rejects.toThrow("Confirm it's you again.");
    await expect(approveAct("stop", production, "web", [copy], now + 46 * 60_000)).rejects.toThrow("Confirm it's you again.");
  });
});

describe("approveProposal", () => {
  afterEach(() => void vi.unstubAllGlobals());
  const web = { machine: "22222222222222222222222222222222", container: "c".repeat(64), hook: false };
  const hook = { machine: "22222222222222222222222222222222", container: "d".repeat(64), hook: true };
  const db = { machine: "11111111111111111111111111111111", container: "e".repeat(64), hook: false };
  const bytes = (text: string) => new TextEncoder().encode(text).buffer;
  const passkey = async (now: number) => {
    vi.stubGlobal("location", { hostname: "localhost" });
    vi.stubGlobal("navigator", { credentials: { get: async () => ({
      rawId: bytes("credential"),
      response: { authenticatorData: bytes("data"), clientDataJSON: bytes("{}"), signature: bytes("signature") },
    }) } });
    await startSession(production, now);
  };
  const steps = [
    { verb: "stop" as const, runtime: "web", copies: [web, hook] },
    { verb: "restart" as const, runtime: "db", copies: [db] },
    { verb: "start" as const, runtime: "web", copies: [web, hook] },
  ];

  it("signs one bundle listing every step's digest in order, and leaves each step unsigned inside it", async () => {
    const now = Date.now();
    await passkey(now);
    const { bundle, approvals } = await approveProposal(steps, production, now);
    expect(bundle.signatures).toHaveLength(1);
    expect(bundle.grants).toHaveLength(1);
    const plan = JSON.parse(bundle.body) as { operation: string; environment: string; cluster: string; steps: { index: number; kind: string; digest: string }[]; issued_at: number; expires_at: number };
    expect(plan).toMatchObject({ environment: "production", cluster: production.cluster, issued_at: now, expires_at: now + 15 * 60_000 });
    // Step 0 stops web and its hook, step 1 restarts db, step 2 starts web alone: a hook never starts.
    expect(plan.steps.map((step) => [step.index, step.kind])).toEqual([[0, "stop"], [1, "stop"], [2, "restart"], [3, "start"]]);
    expect(Object.keys(approvals)).toEqual(["0", "1", "2"]);
    expect(Object.keys(approvals["2"] ?? {})).toEqual([web.container]);
    const envelopes = [approvals["0"]?.[web.container], approvals["0"]?.[hook.container], approvals["1"]?.[db.container], approvals["2"]?.[web.container]];
    for (const [index, envelope] of envelopes.entries()) {
      if (envelope === undefined) expect.fail(`no envelope at ${index}`);
      expect(envelope.signatures).toEqual([]);
      expect(envelope.proposal).toEqual(bundle);
      expect(await stepDigest(envelope.body)).toBe(plan.steps[index]?.digest);
      expect(JSON.parse(envelope.body)).toMatchObject({ proposal: { operation: plan.operation, index }, operation: plan.operation, issued_at: now, expires_at: now + 15 * 60_000 });
    }
    expect(JSON.parse(envelopes[1]?.body ?? "{}")).toMatchObject({ kind: "stop", container: hook.container });
    expect(JSON.parse(envelopes[2]?.body ?? "{}")).toMatchObject({ kind: "restart", service: "db", machines: [db.machine] });
  });

  it("signs nothing for a scope the session doesn't cover, or a plan with nothing to run", async () => {
    const now = Date.now();
    await passkey(now);
    await expect(approveProposal(steps, { ...production, namespace: "fix-api" }, now)).rejects.toThrow("Confirm it's you again.");
    await expect(approveProposal([{ verb: "stop", runtime: "web", copies: [{ ...web, machine: "33333333333333333333333333333333" }] }], production, now)).rejects.toThrow("Confirm it's you again.");
    await expect(approveProposal([{ verb: "start", runtime: "web", copies: [hook] }], production, now)).rejects.toThrow("This proposal has nothing to run.");
  });

  it("refuses a step a Server couldn't take before it prompts, even with a live session", async () => {
    const now = Date.now();
    await passkey(now);
    const get = vi.fn();
    vi.stubGlobal("navigator", { credentials: { get } });
    const environment = { environmentId: "env-production", environmentName: "production", namespace: "production" };
    const held = { kind: "held", public_key: production.cluster } as const;
    const server = (capabilities: string[]) => ({ id: web.machine, name: "machine-2", sealing: held, deviceEnforcement: true, capabilities });
    const target = (capabilities: string[]) => ({
      environment,
      steps: [{ verb: "stop" as const, runtime: "web", copies: [{ id: web.container, machineId: web.machine, hook: false }] }],
      machines: [server(capabilities)],
    });
    await expect(proposalApprovals(target(["ployz.restart.session-grant.v2", "ployz.restart.start-approval.v1"]))).rejects.toThrow("machine-2 needs a newer Ployz to stop it.");
    await expect(proposalApprovals(target(["ployz.restart.session-grant.v2", "ployz.restart.proposal.v1"]))).rejects.toThrow("machine-2 needs a newer Ployz to stop it.");
    expect(get).not.toHaveBeenCalled();
  });

  it("always prompts for a proposal, even while a session covers a single press", async () => {
    const now = Date.now();
    await passkey(now);
    expect(await ensureApproval(production, "restart")).toBe(true);
    // An ask that prompts waits on the dialog, and the next prompting ask cancels it: the one way it settles with nobody pressing.
    const prompted = ensureApproval(production, "proposal");
    void ensureApproval(production, "proposal");
    expect(await prompted).toBe(false);
  });
});

describe("deployScope", () => {
  const held = { kind: "held", public_key: production.cluster } as const;
  const other = { kind: "held", public_key: "other" } as const;
  const environment = { environmentId: production.environmentId, environmentName: "production", namespace: "production" };
  const takes = ["ployz.container.deploy-approval.v1", "ployz.restart.session-grant.v2"];
  const server = (id: string, sealing: typeof held | typeof other, deviceEnforcement: boolean, capabilities = takes) =>
    ({ id: id.repeat(32), name: `machine-${id}`, sealing, deviceEnforcement, capabilities });

  it("names every Server of the Cluster that runs the Environment, since a Deploy may place on any of them", () => {
    const machines = [server("2", held, true), server("1", held, false), server("3", other, true), server("4", held, false)];
    expect(deployScope(environment, ["2".repeat(32)], machines)).toEqual({
      enforced: true,
      scope: { ...environment, cluster: production.cluster, machines: ["1".repeat(32), "2".repeat(32), "4".repeat(32)] },
      older: null,
    });
  });

  it("takes the only Cluster for an Environment that runs nowhere yet, and asks nothing where no Server enforces", () => {
    expect(deployScope(environment, [], [server("1", held, true)]).scope?.machines).toEqual(["1".repeat(32)]);
    expect(deployScope(environment, [], [server("1", held, false)]).enforced).toBe(false);
    expect(deployScope(environment, [], [server("1", held, true), server("3", other, false)]).scope).toBeNull();
  });

  it("names a Server that enforces but can't take a deploy from a browser, and refuses before any prompt", async () => {
    const get = vi.fn();
    vi.stubGlobal("navigator", { credentials: { get } });
    const older = [server("1", held, true, ["ployz.container.deploy-approval.v1"]), server("2", held, false, [])];
    expect(deployScope(environment, [], older).older).toBe("machine-1");
    expect(deployScope(environment, [], [server("2", held, true), server("1", held, false, [])]).older).toBeNull();
    await expect(deployApprovalScope({ environment, environmentMachines: [], machines: older }))
      .rejects.toThrow("machine-1 needs a newer Ployz to deploy from the dashboard. Upgrade it, or run ployz deploy.");
    expect(get).not.toHaveBeenCalled();
    vi.unstubAllGlobals();
  });
});

describe("approveDeploy", () => {
  afterEach(() => void vi.restoreAllMocks());
  afterEach(() => void vi.unstubAllGlobals());
  const bytes = (text: string) => new TextEncoder().encode(text).buffer;
  const credential = vi.fn(async () => ({
    rawId: bytes("credential"),
    response: { authenticatorData: bytes("data"), clientDataJSON: bytes("{}"), signature: bytes("signature") },
  }));
  // The namespace of `PayloadDigest`'s golden fixture in ployz-core's authority/deploy_tests.rs.
  const app: ApprovalScope = { ...production, namespace: "app" };
  const passkey = async (now: number) => {
    vi.stubGlobal("location", { hostname: "localhost" });
    vi.stubGlobal("navigator", { credentials: { get: credential } });
    await startSession(app, now);
  };
  const image = (last: string) => `nginx@sha256:${"0".repeat(63)}${last}`;
  // That fixture, and its digest there: the browser must reproduce it, as every implementation of `PayloadDigest::of` must.
  const GOLDEN = "cb8d3c6ef656772fbe9764188fc4ff7b7059e682de60a7686263ab386cd4ffa5";
  const SEALED = "QUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQQ==";
  const web = (last: string) => ({
    name: "web",
    mode: { mode: "replicated", replicas: 2 },
    container: {
      image: image(last),
      pull_policy: "missing",
      command: ["nginx", "-g", "daemon off;"],
      entrypoint: ["/docker-entrypoint.sh"],
      environment: { PLAIN: "one", SECRET: { sealed: SEALED, scope: "shared", key: "SECRET" } },
      sealed_identity: {
        organization: "org",
        environment: "11111111-1111-1111-1111-111111111111",
        service: "22222222-2222-2222-2222-222222222222",
      },
      cap_add: ["NET_ADMIN"],
      resources: { devices: [{ machine_path: "/dev/null", container_path: "/dev/null", cgroup_permissions: "rwm" }] },
      sysctls: { "net.core.somaxconn": "1024" },
      config_mounts: [{ config_name: "site", target: "/etc/site.conf", mode: 420 }],
    },
    ports: [
      { mode: "host", bind: { kind: "all" }, published_port: 8080, container_port: 80, transport_protocol: "tcp" },
      { mode: "ingress", hostname: "web.example.com", load_balancer_port: 443, container_port: 80, http_protocol: "http" },
    ],
    volumes: [
      { reference: "host", source: { kind: "bind", machine_path: "/srv/web" } },
      { reference: "data", source: { kind: "ordinary", name: "data", driver: { name: "local", options: { type: "tmpfs" } } } },
    ],
    mounts: [{ volume: "host", target: "/srv" }, { volume: "data", target: "/data" }],
    configs: [{ name: "site", content: [104, 105] }],
    pre_deploy: { command: ["migrate"], privileged: false },
    update: { order: "start_first" },
  });
  // SAFETY: the fixture is a spec as the Store writes it on the wire, serde's defaults left out; ployz-core parses it.
  const plan = (target: readonly object[], namespace = "app", version = "v1"): DeployPlan => ({ version, namespace, target: JSON.parse(JSON.stringify(target)) as RequestedServiceSpec[] });
  const deployment = "0f8fad5b-d9cb-469f-a165-70867728950e";
  const verifies = async (signed: { body: string; signatures: { key: string; signature: string }[] }, body: string) => {
    const { key, signature } = signed.signatures[0] ?? expect.fail("unsigned");
    const raw = (text: string) => Uint8Array.from(text.match(/../g) ?? [], (pair) => parseInt(pair, 16));
    const publicKey = await crypto.subtle.importKey("raw", raw(key), { name: "Ed25519" }, false, ["verify"]);
    return crypto.subtle.verify({ name: "Ed25519" }, publicKey, raw(signature), signingInput(body));
  };

  it("builds the body itself, naming the digest ployz-core takes of the reviewed spec, and shows what it signed", async () => {
    const now = Date.now();
    await passkey(now);
    const { approval, lines } = await approveDeploy(app, deployment, "v1", plan([web("1")]), now);
    expect(JSON.parse(approval.body)).toEqual({
      cluster: app.cluster,
      namespace: "app",
      operation: "0f8fad5bd9cb469fa16570867728950e",
      machines: [...app.machines].sort(),
      services: [{ name: "web", digest: GOLDEN }],
      issued_at: now,
      expires_at: now + 15 * 60_000,
      signer: { browser: approval.signatures[0]?.key },
    });
    expect(canonicalJson(JSON.parse(approval.body) as JsonValue)).toBe(approval.body);
    expect(JSON.parse(approval.grants?.[0]?.body ?? "{}")).toMatchObject({ families: ["deploy", "restart", "start", "stop"] });
    expect(lines[0]).toBe("Approved web: nginx@sha256:0000000000000000000000000000000000000000000000000000000000000001 (device /dev/null:/dev/null, adds NET_ADMIN, sysctl net.core.somaxconn, host port 8080, binds /srv/web, hook migrate)");
    expect(lines).toContain('  command ["nginx","-g","daemon off;"]');
    expect(await verifies(approval, approval.body)).toBe(true);
  });

  it("signs the reviewed spec A's digest, so its signature never covers a spec B Cloud swaps in after", async () => {
    const now = Date.now();
    await passkey(now);
    const a = await approveDeploy(app, deployment, "v1", plan([web("1")]), now);
    const b = await approveDeploy(app, deployment, "v1", plan([web("2")]), now);
    // SAFETY: both bodies are canonical DeployApprovals.
    const digest = (body: string) => (JSON.parse(body) as { services: { digest: string }[] }).services[0]?.digest;
    expect(digest(a.approval.body)).toBe(GOLDEN);
    expect(digest(b.approval.body)).not.toBe(GOLDEN);
    expect(await verifies(a.approval, b.approval.body)).toBe(false);
  });

  it("refuses before any prompt or signature a plan of another version or Environment, a tag, or an uncovered scope", async () => {
    const now = Date.now();
    await passkey(now);
    credential.mockClear();
    const sign = vi.spyOn(crypto.subtle, "sign");
    await expect(approveDeploy(app, deployment, "v2", plan([web("1")]), now)).rejects.toThrow("The Environment changed after this review.");
    await expect(approveDeploy(app, deployment, "v1", plan([web("1")], "production"), now)).rejects.toThrow("The Environment changed after this review.");
    const tagged = { ...web("1"), container: { ...web("1").container, image: "nginx:1.29-alpine" } };
    await expect(approveDeploy(app, deployment, "v1", plan([tagged]), now))
      .rejects.toThrow("The dashboard can't check what web's tag nginx:1.29-alpine points to. Write the image by digest, or run ployz deploy, which pins it for you.");
    await expect(approveDeploy(production, deployment, "v1", plan([web("1")], "production"), now)).rejects.toThrow("Confirm it's you again.");
    expect(sign).not.toHaveBeenCalled();
    expect(credential).not.toHaveBeenCalled();
    // A review the Store refused once, with its loss mark, still names the plan's version.
    await expect(approveDeploy(app, deployment, "v1:accepted#loss-x", plan([web("1")]), now)).resolves.toBeDefined();
  });
});
