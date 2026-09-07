import { describe, expect, it } from "vitest";
import type { MachineId } from "@ployz/sdk";
import type { DataLossIdentity } from "#/modules/runtime/data-loss-identity";
import { toMachineRemoveAttemptView } from "./machine-removal";

const entry = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" as MachineId;

describe("toMachineRemoveAttemptView", () => {
  const base = {
    id: "11111111-1111-4111-8111-111111111111",
    machineId: entry,
    missingIdentities: null,
    failureMessage: null,
  };

  it("projects active and succeeded attempts without extra nullable fields", () => {
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
        failureMessage: "sdk_not_shipped",
      }),
    ).toEqual({
      id: base.id,
      machineId: entry,
      state: "failed",
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
