import { afterEach, describe, expect, it, vi } from "vitest";
import {
  actScope,
  approveAct,
  approveProposal,
  type ApprovalScope,
  type ApprovalSession,
  canonicalJson,
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
import { actApprovals, ensureApproval, proposalApprovals } from "./approval-prompt";

const production: ApprovalScope = {
  environmentId: "env-production",
  environmentName: "production",
  namespace: "production",
  cluster: "CQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQk=",
  machines: ["22222222222222222222222222222222", "11111111111111111111111111111111"],
};

const session = (scope: ApprovalScope, expiresAt: number): ApprovalSession =>
  ({ scope, families: ["restart", "start", "stop"], issuedAt: expiresAt - 3_600_000, expiresAt, keyCode: "abcdefghij" });

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
      `{"browser_key":"${browserKey}","cluster":"${production.cluster}","delegation":"none","environment":"production","expires_at":4600000,`
        + `"families":["restart","start","stop"],"issued_at":1000000,"machines":["11111111111111111111111111111111","22222222222222222222222222222222"]}`,
    );
    const rustBody = body.replace(`"families":["restart","start","stop"]`, `"families":["restart","stop"]`);
    expect(hex(await grantChallenge(rustBody))).toBe("3f6c4b9b3dc458f09a5b0ec4dd6b15a4e0794a22641b2b0ecc2523002b56888a");
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

  it("names the copies' Cluster and the Servers in it that run the Environment, once a host enforces", () => {
    const result = actScope(environment, [{ machineId: "22222222222222222222222222222222" }], [
      { id: "22222222222222222222222222222222", sealing: held, deviceEnforcement: true },
      { id: "11111111111111111111111111111111", sealing: held },
      { id: "33333333333333333333333333333333", sealing: { kind: "held", public_key: "other" } },
      { id: "44444444444444444444444444444444", sealing: held },
    ], ["11111111111111111111111111111111", "22222222222222222222222222222222", "33333333333333333333333333333333"]);
    expect(result).toEqual({
      enforced: true,
      scope: { ...environment, cluster: production.cluster, machines: ["11111111111111111111111111111111", "22222222222222222222222222222222"] },
    });
  });

  it("asks nothing of Servers that don't enforce", () => {
    expect(actScope(environment, [{ machineId: "m" }], [{ id: "m", sealing: held, deviceEnforcement: false }], ["m"]).enforced).toBe(false);
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
      expect(JSON.parse(signed.grants?.[0]?.body ?? "{}")).toMatchObject({ families: ["restart", "start", "stop"], environment: "production" });
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
      machines: [{ id: copy.machine, name: "machine-2", sealing: held, deviceEnforcement: true, capabilities: ["ployz.restart.session-grant.v1"] }],
      environmentMachines: [copy.machine],
    };
    const { scope } = actScope(environment, target.copies, target.machines, target.environmentMachines);
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
      environmentMachines: [web.machine],
    });
    await expect(proposalApprovals(target(["ployz.restart.session-grant.v1", "ployz.restart.start-approval.v1"]))).rejects.toThrow("machine-2 needs a newer Ployz to stop it.");
    await expect(proposalApprovals(target(["ployz.restart.session-grant.v1", "ployz.restart.proposal.v1"]))).rejects.toThrow("machine-2 needs a newer Ployz to stop it.");
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
