import { describe, expect, it } from "vitest";
import type { MachineId } from "@ployz/sdk";
import type { DataLossIdentity } from "#/modules/runtime/data-loss-identity";
import { requirements, ServerLacks } from "#/modules/runtime/daemon-compat";
import { removalFailureMessage, removalLack, toMachineRemoveAttemptView } from "./machine-removal";

const entry = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" as MachineId;

describe("toMachineRemoveAttemptView", () => {
  const base = {
    id: "11111111-1111-4111-8111-111111111111",
    machineId: entry,
    missingIdentities: null,
    failureCode: null,
    failureMessage: null,
    result: null,
  };

  it("projects a succeeded attempt with its result", () => {
    const result = { resetWarning: null, release: { kind: "released" as const } };
    expect(toMachineRemoveAttemptView({ ...base, state: "succeeded", result })).toEqual({
      id: base.id,
      machineId: entry,
      state: "succeeded",
      result,
    });
  });

  it("projects active attempts without extra nullable fields", () => {
    expect(toMachineRemoveAttemptView({ ...base, state: "pending" })).toEqual({
      id: base.id,
      machineId: entry,
      state: "pending",
    });
    expect(toMachineRemoveAttemptView({ ...base, state: "running" })).toEqual({
      id: base.id,
      machineId: entry,
      state: "running",
    });
  });

  it("round-trips rust missing identities for the shared Data Loss modal", () => {
    const identities: DataLossIdentity[] = [
      { kind: "docker_volume", id: { machine_id: entry, name: "data" } },
    ];
    expect(
      toMachineRemoveAttemptView({
        ...base,
        state: "missing_identities",
        missingIdentities: identities,
      }),
    ).toEqual({
      id: base.id,
      machineId: entry,
      state: "missing_identities",
      missingIdentities: identities,
    });
  });

  it("surfaces a terminal failure without a confirm-everything helper", () => {
    expect(
      toMachineRemoveAttemptView({
        ...base,
        state: "failed",
        failureCode: "server_lacks",
        failureMessage: "sdk_not_shipped",
      }),
    ).toEqual({
      id: base.id,
      machineId: entry,
      state: "failed",
      failureCode: "server_lacks",
      failureMessage: "sdk_not_shipped",
    });
  });

  it("does not treat an empty missing-identities row as confirm-everything", () => {
    expect(() =>
      toMachineRemoveAttemptView({
        ...base,
        state: "missing_identities",
        missingIdentities: [],
      }),
    ).toThrow("missing_identities attempt has no identities.");
  });
});

describe("removalLack", () => {
  const listed = [requirements.leave.capability, requirements.forget.capability];
  const lacking = ["ployz.machine.inspect.v1"];
  const removal = { target: "target", entries: ["entry", "target"], noReset: false };

  it("asks when every record lists the steps or lists nothing", () => {
    expect(removalLack([{ id: "target", capabilities: listed }, { id: "entry", capabilities: listed }], removal)).toBeNull();
    expect(removalLack([{ id: "target", capabilities: [] }, { id: "entry", capabilities: [] }], removal)).toBeNull();
  });

  it("names Leave on a target that lacks it, and skips it without a reset", () => {
    const servers = [{ id: "target", capabilities: lacking }, { id: "entry", capabilities: listed }];
    expect(removalLack(servers, removal)).toEqual(new ServerLacks({ machineId: "target", capability: "ployz.machine.leave.v1" }));
    expect(removalLack(servers, { ...removal, noReset: true })).toBeNull();
  });

  it("names Forget on an entry that lacks it", () => {
    const servers = [{ id: "target", capabilities: listed }, { id: "entry", capabilities: lacking }];
    expect(removalLack(servers, removal)).toEqual(new ServerLacks({ machineId: "entry", capability: "ployz.machine.forget.v1" }));
  });
});

describe("removalFailureMessage", () => {
  it("leaves a removal whose retries ran out unknown, and names Forget for a Server that is gone", () => {
    const message = removalFailureMessage("web-2", {
      failureCode: "retry_exhausted",
      failureMessage: "Machine remove retries were exhausted.",
    });
    expect(message).toContain("web-2");
    expect(message).toContain("Forget server");
    expect(message).not.toMatch(/removed/i);
  });

  it("says a refusal as the Server gave it", () => {
    expect(removalFailureMessage("web-2", { failureCode: "conflict", failureMessage: "A copy is still writing." }))
      .toBe("A copy is still writing.");
  });
});
