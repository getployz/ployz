import { describe, expect, it } from "vitest";
import {
  orderDialEntries,
} from "#/modules/runtime/dial-entry";

describe("orderDialEntries", () => {
  it("puts the preferred enrolled machine first, then the rest in enrolled order", () => {
    expect(
      orderDialEntries({
        preferredMachineId: "b",
        enrolledMachineIds: ["a", "b", "c"],
      }),
    ).toEqual(["b", "a", "c"]);
  });

  it("skips a preferred machine that is not enrolled", () => {
    expect(
      orderDialEntries({
        preferredMachineId: "missing",
        enrolledMachineIds: ["a", "b"],
      }),
    ).toEqual(["a", "b"]);
  });

  it("keeps enrolled order when there is no preferred machine", () => {
    expect(
      orderDialEntries({
        preferredMachineId: null,
        enrolledMachineIds: ["a", "b"],
      }),
    ).toEqual(["a", "b"]);
  });

  it("dedupes enrolled ids and returns nothing when none are enrolled", () => {
    expect(
      orderDialEntries({
        preferredMachineId: "a",
        enrolledMachineIds: ["a", "a", "b"],
      }),
    ).toEqual(["a", "b"]);
    expect(
      orderDialEntries({
        preferredMachineId: "a",
        enrolledMachineIds: [],
      }),
    ).toEqual([]);
  });
});
