import { describe, expect, it } from "vitest";
import {
  environmentSavedStateBasisMatches,
  environmentSavedStateDiscardCommandSchema,
} from "#/modules/environment-design/saved-state";
import { decodeStrict, isValid } from "#/modules/environment-design/schema";

const savedStateSnapshotId = "00000000-0000-4000-8000-000000000001";
const serviceId = "00000000-0000-4000-8000-000000000002";

describe("Environment Saved State commands", () => {
  it("requires one exact basis for one atomic discard command", () => {
    const command = decodeStrict(environmentSavedStateDiscardCommandSchema, {
      kind: "discard",
      basis: { kind: "saved_revision", savedStateSnapshotId },
      operations: [
        { kind: "node", nodeType: "service", nodeId: serviceId },
      ],
    });

    expect(
      environmentSavedStateBasisMatches(command.basis, savedStateSnapshotId),
    ).toBe(true);
    expect(
      environmentSavedStateBasisMatches(
        command.basis,
        "00000000-0000-4000-8000-000000000003",
      ),
    ).toBe(false);
    expect(
      isValid(environmentSavedStateDiscardCommandSchema, {
        ...command,
        operations: [],
      }),
    ).toBe(false);
    expect(
      isValid(environmentSavedStateDiscardCommandSchema, {
        ...command,
        operations: [command.operations[0], command.operations[0]],
      }),
    ).toBe(false);
  });
});
