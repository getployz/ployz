import { describe, expect, it } from "vitest";
import { callerEnvironments, callerPermissions } from "#/modules/identity/actor";
import { ployzVia } from "#/modules/identity/caller.server";
import { FULL_GRANT, grantLabel, presetGrant, type TokenPreset } from "#/modules/identity/token-grant";
import { permissionsOf } from "#/modules/organization/permissions";

describe("ployzVia", () => {
  it.each([
    ["cli", undefined, "cli"],
    ["cli", "cli", "cli"],
    ["cli", "agent", "agent"],
    ["cli", "dashboard", "cli"],
    ["agent", undefined, "agent"],
    ["agent", "cli", "agent"],
    ["agent", "dashboard", "agent"],
    ["agent", "agent", "agent"],
  ] as const)("a %s credential with x-ployz-via %s speaks as %s", (credential, header, via) => {
    const headers = new Headers(header === undefined ? {} : { "x-ployz-via": header });
    expect(ployzVia(credential, headers)).toBe(via);
  });
});

const READS = ["org:read", "members:read", "project:read", "variables:read", "logs:read", "metrics:read", "volumes:read", "servers:read"] as const;

const tokenOf = (role: "owner" | "admin" | "member", choice: TokenPreset) =>
  ({ role, support: null, credential: { kind: "token", id: "t", grant: presetGrant(choice) } }) as const;

describe("a token's effective permissions", () => {
  it.each([
    ["member", { preset: "full" }, permissionsOf("member")],
    ["admin", { preset: "full" }, permissionsOf("admin")],
    ["owner", { preset: "full" }, permissionsOf("owner")],
    ["member", { preset: "read" }, READS],
    ["admin", { preset: "read" }, [...READS, "audit:read"]],
    ["owner", { preset: "read" }, [...READS, "audit:read"]],
    ["member", { preset: "deploy", environment: "staging" }, [...READS, "changes:stage", "changes:deploy"]],
    ["admin", { preset: "deploy", environment: "staging" }, [...READS, "audit:read", "changes:stage", "changes:deploy"]],
    ["owner", { preset: "deploy", environment: "staging" }, [...READS, "audit:read", "changes:stage", "changes:deploy"]],
  ] as const)("a %s's %o token holds its role narrowed by the preset", (role, choice, expected) => {
    expect(new Set(callerPermissions(tokenOf(role, choice)))).toEqual(new Set(expected));
  });

  it("a Deploy token writes only its Environment, and the others write any", () => {
    expect(callerEnvironments(tokenOf("owner", { preset: "deploy", environment: "staging" }))).toEqual(["staging"]);
    expect(callerEnvironments(tokenOf("owner", { preset: "read" }))).toBeUndefined();
    expect(callerEnvironments(tokenOf("owner", { preset: "full" }))).toBeUndefined();
  });

  it("a Full as you token follows its maker's role as it changes", () => {
    const grant = presetGrant({ preset: "full" });
    expect(callerPermissions({ role: "member", support: null, credential: { kind: "token", id: "t", grant } })).not.toContain("servers:add");
    expect(callerPermissions({ role: "admin", support: null, credential: { kind: "token", id: "t", grant } })).toContain("servers:add");
  });

  it("names each preset for Tokens, and an unmigrated grant as Full as you", () => {
    expect(grantLabel(FULL_GRANT)).toBe("Full as you");
    expect(grantLabel(presetGrant({ preset: "read" }))).toBe("Read only");
    expect(grantLabel(presetGrant({ preset: "deploy", environment: "staging" }))).toBe("Deploy staging");
  });
});
