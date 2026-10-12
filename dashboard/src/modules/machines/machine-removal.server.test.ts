import { describe, expect, it } from "vitest";
import type { MachineId } from "@ployz/sdk";
import { Effect } from "effect";
import { MissingDataLossIdentities } from "#/modules/runtime/data-loss-confirm";
import { PloyzProviderError } from "#/modules/runtime/ployz.server";
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

  it("ends on the Server's refusal instead of retrying it", async () => {
    const message = "Server two answers and holds copies of Volumes vol-1; removing it without a reset leaves them behind. No changes made.";
    await expect(
      Effect.runPromise(
        asRemoveMachineOutcome(
          Effect.fail(new PloyzProviderError({ operation: "remove machine membership", cause: { code: "conflict", message } })),
        ),
      ),
    ).resolves.toEqual({ kind: "refused", failureCode: "conflict", message });
  });

  it("leaves any other failure to the step's retries", async () => {
    const exit = await Effect.runPromiseExit(
      asRemoveMachineOutcome(
        Effect.fail(new PloyzProviderError({ operation: "remove machine membership", cause: { code: "unavailable", message: "transport error" } })),
      ),
    );
    expect(exit._tag).toBe("Failure");
  });
});
