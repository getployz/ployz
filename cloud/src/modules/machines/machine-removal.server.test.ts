import { describe, expect, it } from "vitest";
import type { MachineId } from "@ployz/sdk";
import { Effect } from "effect";
import { MissingDataLossIdentities } from "#/modules/runtime/data-loss-confirm";
import { SdkSurfaceNotShipped } from "#/modules/runtime/ployz.server";
import { asRemoveMachineOutcome } from "./machine-removal.server";

const identities = [
  {
    kind: "docker_volume" as const,
    id: {
      machine_id: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" as MachineId,
      name: "data",
    },
  },
];

describe("asRemoveMachineOutcome", () => {
  it("maps rust missing identities onto the Data Loss reopen path", async () => {
    await expect(
      Effect.runPromise(
        asRemoveMachineOutcome(
          Effect.fail(new MissingDataLossIdentities(identities)),
        ),
      ),
    ).resolves.toEqual({ kind: "missing_identities", identities });
  });

  it("maps a disconnected adapter onto a permanent Cloud failure", async () => {
    await expect(
      Effect.runPromise(
        asRemoveMachineOutcome(
          Effect.fail(
            new SdkSurfaceNotShipped({
              surface: "removeMachine",
              ticket: "getployz/ployz2#253",
            }),
          ),
        ),
      ),
    ).resolves.toEqual({
      kind: "permanent_failure",
      failureCode: "sdk_not_shipped",
      failureMessage:
        "removeMachine is not shipped in @ployz/sdk yet (getployz/ployz2#253)",
    });
  });
});
