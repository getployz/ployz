import { describe, expect, it } from "vitest";
import { activationGate, type ActivationReport, type ActivationServer } from "./activation";

const IDENTITY = "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=";
const RECIPIENT = "AgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgI=";
const OPERATION = "a".repeat(32);

const report = (state: ActivationReport["state"], extra: Partial<ActivationReport> = {}): ActivationReport => ({
  state,
  operation: state === "awaiting" ? null : OPERATION,
  legacyMembers: [],
  legacyKeys: [],
  privateAccess: null,
  ...extra,
});

const server = (digit: string, name: string, extra: Partial<ActivationServer> = {}): ActivationServer => ({
  id: digit.repeat(32),
  name,
  answered: true,
  enforces: true,
  member: { id: digit.repeat(32), public_key: Array.from({ length: 32 }, () => Number(digit)), subnet: `10.210.${digit}.0/24` },
  identity: IDENTITY,
  recipient: null,
  activation: report("awaiting"),
  ...extra,
});

describe("activationGate", () => {
  it("says nothing while no Server runs a Ployz that can activate", () => {
    expect(activationGate([server("1", "web-1", { enforces: false, activation: null })])).toEqual({ kind: "none" });
  });

  it("offers the passkey path over every Server when all await activation with nothing added while it was off", () => {
    const servers = [server("1", "web-1"), server("2", "web-2")];
    expect(activationGate(servers)).toEqual({ kind: "offer", cluster: IDENTITY, members: servers.map((one) => one.member) });
  });

  it("names a Server on an older Ployz instead of offering, as the CLI leaves it out", () => {
    expect(activationGate([server("1", "web-1"), server("2", "web-2", { enforces: false, activation: null })]))
      .toEqual({ kind: "blocked", blockers: [{ kind: "older", servers: ["web-2"] }] });
  });

  it("names Members and keys added while enforcement was off, by name and code", () => {
    const gate = activationGate([
      server("1", "web-1", { activation: report("awaiting", { legacyMembers: ["2".repeat(32)] }) }),
      server("2", "web-2", { activation: report("awaiting", { legacyKeys: [{ code: "K7Q2", kind: "device", account: null }] }) }),
    ]);
    expect(gate).toEqual({ kind: "blocked", blockers: [
      { kind: "legacy-members", servers: ["web-1"], members: ["web-2"] },
      { kind: "legacy-keys", servers: ["web-2"], keys: [{ code: "K7Q2", kind: "device", account: null }] },
    ] });
  });

  it("doesn't offer while a Server doesn't answer, or the Servers hold different keys", () => {
    expect(activationGate([server("1", "web-1"), server("2", "web-2", { answered: false, member: null, identity: null, activation: null, enforces: false })]))
      .toMatchObject({ kind: "blocked", blockers: [{ kind: "unanswered", servers: ["web-2"] }] });
    expect(activationGate([server("1", "web-1"), server("2", "web-2", { identity: RECIPIENT })]))
      .toEqual({ kind: "blocked", blockers: [{ kind: "no-cluster-key" }] });
  });

  it("shows a pending activation as pending, never as something to start again", () => {
    expect(activationGate([server("1", "web-1", { activation: report("activated") }), server("2", "web-2", { activation: report("activating") })]))
      .toEqual({ kind: "activating", servers: ["web-2"] });
  });

  it("offers the new key once every Server activated under one set and none holds one, and nothing after", () => {
    const first = server("1", "web-1", { activation: report("activated") });
    const second = server("2", "web-2", { activation: report("activated") });
    expect(activationGate([first, second])).toEqual({ kind: "keyless", cluster: IDENTITY, operation: OPERATION, members: [first.member, second.member] });
    expect(activationGate([{ ...first, recipient: RECIPIENT }, second])).toEqual({ kind: "activated" });
  });

  it("names Servers already activated beside ones that aren't", () => {
    expect(activationGate([server("1", "web-1", { activation: report("activated") }), server("2", "web-2")]))
      .toEqual({ kind: "blocked", blockers: [{ kind: "partial", servers: ["web-1"] }] });
  });
});
