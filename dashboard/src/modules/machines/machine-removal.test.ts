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
    removeSentAt: null,
    noReset: false,
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
      unfinished: false,
      failureMessage: "sdk_not_shipped",
    });
  });

  it("calls a removal unfinished only when a sent reset ran out of retries", () => {
    const sent = new Date();
    const exhausted = { ...base, state: "failed" as const, failureCode: "retry_exhausted", failureMessage: "Machine remove retries were exhausted." };
    const unfinished = (attempt: Parameters<typeof toMachineRemoveAttemptView>[0]) => {
      const view = toMachineRemoveAttemptView(attempt);
      return view.state === "failed" && view.unfinished;
    };
    expect(unfinished({ ...exhausted, removeSentAt: sent })).toBe(true);
    // Nothing reached the Server before the retries ran out.
    expect(unfinished(exhausted)).toBe(false);
    // A forget never asks for a reset.
    expect(unfinished({ ...exhausted, removeSentAt: sent, noReset: true })).toBe(false);
    expect(unfinished({ ...exhausted, state: "cancelled", removeSentAt: sent })).toBe(false);
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
  const sent = new Date();
  // The row onFailure leaves when any step runs out of retries. Once the reset was sent, a step after the Server's reply
  // (release-server, say) leaves the same row as remove-machine whose reply never came.
  const exhausted = {
    id: "11111111-1111-4111-8111-111111111111",
    machineId: entry,
    state: "failed" as const,
    missingIdentities: null,
    failureCode: "retry_exhausted",
    failureMessage: "Machine remove retries were exhausted.",
    removeSentAt: sent,
    noReset: false,
    result: null,
  };
  const said = (attempt: Parameters<typeof toMachineRemoveAttemptView>[0]) => {
    const view = toMachineRemoveAttemptView(attempt);
    if (view.state !== "failed" && view.state !== "cancelled") throw new Error(`${view.state} is not a failure.`);
    return removalFailureMessage("web-2", view);
  };
  // What the row can't back: the Server may have answered, reset and been dropped by Cloud before a later step ran out.
  const unbacked = /unknown|couldn't tell|answered|it's still listed|Cloud still lists/i;

  it("says a sent reset didn't finish, and names Forget only for a Server that is gone and still listed", () => {
    const message = said(exhausted);
    expect(message).toContain("Removing web-2 didn't finish");
    expect(message).toContain("If web-2 is gone but still listed, forget it with Forget server.");
    expect(message).not.toMatch(unbacked);
    expect(message).not.toMatch(/removed/i);
  });

  it("claims nothing about the reset when a step after the Server's reply ran out of retries", () => {
    // release-server ran out after remove-machine's reply was memoized: the reset is confirmed, and the row may be gone.
    expect(said(exhausted)).not.toMatch(unbacked);
  });

  it("says the stored failure for a removal that sent nothing, a forget, a cancellation or a refusal", () => {
    expect(said({ ...exhausted, removeSentAt: null })).toBe("Machine remove retries were exhausted.");
    expect(said({ ...exhausted, noReset: true })).toBe("Machine remove retries were exhausted.");
    expect(said({ ...exhausted, state: "cancelled", failureCode: "cancelled", failureMessage: "Machine remove was cancelled." }))
      .toBe("Machine remove was cancelled.");
    expect(said({ ...exhausted, failureCode: "conflict", failureMessage: "A copy is still writing." }))
      .toBe("A copy is still writing.");
  });
});
