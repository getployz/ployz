import { describe, expect, it } from "vitest";
import { Schema } from "effect";
import { rustMachineIdSchema } from "./enrollment";
import { heldMachineIds } from "./enrollment.server";
const preferred = Schema.decodeUnknownSync(rustMachineIdSchema)("a".repeat(32));
const other = Schema.decodeUnknownSync(rustMachineIdSchema)("b".repeat(32));

describe("heldMachineIds", () => {
  it("keeps rows with a usable Machine id and drops the rest", () => {
    expect(
      heldMachineIds([
        { machineId: preferred },
        { machineId: "not-a-machine" },
        { nope: true },
        null,
        "string",
        { machineId: other },
      ]),
    ).toEqual([preferred, other]);
  });

  it("yields nothing when no row carries a usable id", () => {
    // Drives the indeterminate observation: entries exist, none is usable, so
    // the List is neither Dial-able nor evidence that nobody holds a Register.
    expect(heldMachineIds([{ machineId: "not-a-machine" }, {}])).toEqual([]);
  });
});
