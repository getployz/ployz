import type { MachineId } from "@ployz/sdk";
import { Cause, Effect, Exit } from "effect";
import { Inngest } from "inngest";
import { describe, expect, it } from "vitest";
import {
  InngestClient,
  InngestEventSendError,
} from "#/modules/inngest/client";
import {
  dispatchVolumeRemoveRequested,
  volumeRemoveCompletion,
} from "#/modules/runtime/volume-removal.server";

const volume = {
  machine_id: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" as MachineId,
  name: "vol-resource",
};

describe("volume removal outcomes", () => {
  it("fails dispatch so a pending row stays retryable", async () => {
    const failing = new Inngest({ id: "volume-remove-dispatch-fail-test" });
    failing.send = async () => {
      throw new Error("Inngest unavailable");
    };
    const exit = await Effect.runPromise(
      dispatchVolumeRemoveRequested("attempt-1").pipe(
        Effect.provideService(InngestClient, failing),
        Effect.exit,
      ),
    );

    expect(Exit.isFailure(exit)).toBe(true);
    if (Exit.isFailure(exit)) {
      expect(Cause.squash(exit.cause)).toBeInstanceOf(InngestEventSendError);
    }
  });

  it("derives partial completion from the provider's exact identities", () => {
    expect(
      volumeRemoveCompletion(
        { volumes: [volume] },
        {
          destroyed: [],
          failed: [{ ...volume, message: "busy" }],
          omitted: [],
        },
      ),
    ).toEqual({
      status: "partial",
      outcome: {
        destroyed: [],
        failed: [{ ...volume, message: "busy" }],
        omitted: [],
      },
    });
  });
});
