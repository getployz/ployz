import { describe, expect, it } from "vitest";
import {
  type ApprovalScope,
  type ApprovalSession,
  canonicalJson,
  grantBody,
  grantChallenge,
  hex,
  keyCode,
  promptReason,
  restartScope,
  signingInput,
} from "./approval-session";

const production: ApprovalScope = {
  environmentId: "env-production",
  environmentName: "production",
  namespace: "production",
  cluster: "CQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQk=",
  machines: ["22222222222222222222222222222222", "11111111111111111111111111111111"],
};

const session = (scope: ApprovalScope, expiresAt: number): ApprovalSession =>
  ({ scope, families: ["restart"], issuedAt: expiresAt - 3_600_000, expiresAt, keyCode: "abcdefghij" });

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
        + `"families":["restart"],"issued_at":1000000,"machines":["11111111111111111111111111111111","22222222222222222222222222222222"]}`,
    );
    const rustBody = body.replace(`"families":["restart"]`, `"families":["restart","stop"]`);
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
    expect(promptReason(live, production, "stop", now)).toBe("scope");
  });

  it("prompts again once less than one approval's 15 minutes is left, so no approval is cut short", () => {
    expect(promptReason(session(production, now + 15 * 60_000 - 1), production, "restart", now)).toBe("expired");
    expect(promptReason(session(production, now + 15 * 60_000), production, "restart", now)).toBeNull();
  });
});

describe("restartScope", () => {
  const held = { kind: "held", public_key: production.cluster };
  const environment = { environmentId: "env-production", environmentName: "production", namespace: "production" };

  it("names the copies' Cluster and the Servers in it that run the Environment, once a host enforces", () => {
    const result = restartScope(environment, [{ machineId: "22222222222222222222222222222222" }], [
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
    expect(restartScope(environment, [{ machineId: "m" }], [{ id: "m", sealing: held, deviceEnforcement: false }], ["m"]).enforced).toBe(false);
  });
});

describe("signingInput", () => {
  it("prefixes the body as `keys::signing_input` does for a workflow", () => {
    expect(new TextDecoder().decode(signingInput("{}"))).toBe("ployz-authority/v1\nworkflow\n{}");
  });
});
