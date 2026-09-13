import { describe, expect, it } from "vitest";
import type { MachineId } from "@ployz/sdk";
import {
  remainingVolumeIdentities,
  retryVolumesForAttempt,
  volumeRemoveIsBusy,
  volumeRemoveIsRetryable,
  volumeRemoveIsTerminal,
  volumeRemoveStatusFromOutcome,
  volumesConfirmedForPhysicalName,
  type VolumeRemoveOutcome,
} from "./volume-removal";
import { parseVolumeRemoveOutcome } from "./volume-removal-outcome";

const machineA = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" as MachineId;
const machineB = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb" as MachineId;
const volA = { machine_id: machineA, name: "vol-resource" };
const volB = { machine_id: machineB, name: "vol-resource" };

function outcome(
  overrides: Partial<VolumeRemoveOutcome> = {},
): VolumeRemoveOutcome {
  return { destroyed: [], failed: [], omitted: [], ...overrides };
}

describe("volume remove identities", () => {
  it("treats omit and fail as remaining work, not total success", () => {
    const requested = [volA, volB];

    expect(
      volumeRemoveStatusFromOutcome(
        requested,
        outcome({ destroyed: [volA], omitted: [volB] }),
      ),
    ).toBe("partial");
    expect(
      volumeRemoveStatusFromOutcome(
        requested,
        outcome({ destroyed: [volA], failed: [volB] }),
      ),
    ).toBe("partial");
    expect(
      remainingVolumeIdentities(requested, [volA]),
    ).toEqual([volB]);
  });

  it("completes only when every requested identity was destroyed", () => {
    expect(
      volumeRemoveStatusFromOutcome(
        [volA, volB],
        outcome({ destroyed: [volA, volB] }),
      ),
    ).toBe("completed");
    expect(
      volumeRemoveStatusFromOutcome(
        [volA, volB],
        outcome({
          destroyed: [volA, volB],
          failed: [{ ...volA, message: "also failed" }],
        }),
      ),
    ).toBe("partial");
  });

  it("rejects confirm identities whose name is not this volume's physical name", () => {
    expect(
      volumesConfirmedForPhysicalName(
        [
          { kind: "docker_volume", id: volA },
          { kind: "docker_volume", id: { ...volB, name: "other" } },
        ],
        "vol-resource",
      ).error,
    ).toBe("name_mismatch");
    expect(
      volumesConfirmedForPhysicalName(
        [{ kind: "docker_volume", id: volA }],
        "vol-resource",
      ),
    ).toEqual({
      volumes: [volA],
      error: null,
    });
    expect(
      volumesConfirmedForPhysicalName([], "vol-resource").error,
    ).toBe("empty");
  });

  it("retries remaining identities after partial, and the same list after fail or cancel", () => {
    expect(
      retryVolumesForAttempt({
        status: "partial",
        volumes: [volA, volB],
        outcome: outcome({ destroyed: [volA], failed: [volB] }),
      }),
    ).toEqual({ kind: "retry", volumes: [volB] });
    expect(
      retryVolumesForAttempt({
        status: "failed",
        volumes: [volA, volB],
        outcome: null,
      }),
    ).toEqual({ kind: "retry", volumes: [volA, volB] });
    expect(
      retryVolumesForAttempt({
        status: "cancelled",
        volumes: [volA],
        outcome: null,
      }),
    ).toEqual({ kind: "retry", volumes: [volA] });
    expect(
      retryVolumesForAttempt({
        status: "pending",
        volumes: [volA],
        outcome: null,
      }),
    ).toEqual({ kind: "resend" });
    expect(
      retryVolumesForAttempt({
        status: "running",
        volumes: [volA],
        outcome: null,
      }),
    ).toEqual({ kind: "conflict" });
    expect(
      retryVolumesForAttempt({
        status: "completed",
        volumes: [volA],
        outcome: outcome({ destroyed: [volA] }),
      }),
    ).toEqual({ kind: "conflict" });
    expect(volumeRemoveIsBusy("pending")).toBe(true);
    expect(volumeRemoveIsRetryable("pending")).toBe(true);
    expect(volumeRemoveIsRetryable("running")).toBe(false);
    expect(volumeRemoveIsTerminal("partial")).toBe(true);
    expect(volumeRemoveIsTerminal("running")).toBe(false);
  });
});

describe("volume remove outcome parser", () => {
  it("accepts per-volume SDK removal outcomes", () => {
    expect(
      parseVolumeRemoveOutcome(
        [
          { id: volA, outcome: { status: "removed" } },
          { id: volB, outcome: { status: "failed", error: { code: "unavailable", message: "busy", details: null } } },
        ],
        [volA, volB],
      ),
    ).toEqual({
      destroyed: [volA],
      failed: [{ ...volB, message: "busy" }],
      omitted: [],
    });
  });

  it("treats unrecognized returns as omitted requested identities, not success", () => {
    const requested = [volA, volB];
    expect(parseVolumeRemoveOutcome(null, requested)).toEqual({
      destroyed: [],
      failed: [],
      omitted: requested,
    });
    expect(parseVolumeRemoveOutcome({ ok: true }, requested)).toEqual({
      destroyed: [],
      failed: [],
      omitted: requested,
    });
    expect(
      volumeRemoveStatusFromOutcome(
        requested,
        parseVolumeRemoveOutcome({ ok: true }, requested),
      ),
    ).toBe("partial");
  });

  it("does not treat a requested identity as destroyed just because rust omitted others", () => {
    const requested = [volA, volB];
    const parsed = parseVolumeRemoveOutcome(
      [{ id: volA, outcome: { status: "removed" } }],
      requested,
    );
    expect(parsed.omitted).toEqual([volB]);
    expect(volumeRemoveStatusFromOutcome(requested, parsed)).toBe("partial");
  });
});

it("keeps same-machine failures, omissions, and ambiguous results distinct", () => {
  const other = { ...volA, name: "other" };
  expect(parseVolumeRemoveOutcome([
    { id: volA, outcome: { status: "removed" } },
    { id: other, outcome: { status: "failed", error: { code: "conflict", message: "in use", details: null } } },
    { id: volB, outcome: { status: "omitted" } },
  ], [volA, other, volB])).toEqual({
    destroyed: [volA], failed: [{ ...other, message: "in use" }], omitted: [volB],
  });
  expect(parseVolumeRemoveOutcome([
    { id: volA, outcome: { status: "removed" } },
    { id: volA, outcome: { status: "omitted" } },
  ], [volA]).omitted).toEqual([volA]);
});
