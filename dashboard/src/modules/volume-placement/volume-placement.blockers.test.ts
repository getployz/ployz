import { Effect } from "effect";
import { describe, expect, it } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { type ConnectedRuntimeClient, OrganizationRuntime, type OrganizationRuntimeService } from "#/modules/runtime/organization-runtime.server";
import { readPlacementBlockers } from "./volume-placement.deploy.server";

const machine = (name: string, daemonVersion: string) => ({ machine: { name, runtime: { daemon_version: daemonVersion } }, storage: { state: "pool" } });

type Frame = { readonly machines: ReturnType<typeof machine>[]; readonly incomplete_ids: { readonly machines: string[] } };

const reading = (frame: Frame) => readPlacementBlockers("org-1").pipe(
  Effect.provideService(OrganizationRuntime, asTestDouble<OrganizationRuntimeService>()({
    cancel: () => Effect.void,
    open: () => Effect.succeed({ status: "connected" as const, connected: asTestDouble<ConnectedRuntimeClient>()({
      watchFirstFrame: () => Effect.succeed(frame),
    }) }),
  })),
  Effect.result,
  Effect.runPromise,
);

describe("readPlacementBlockers", () => {
  it("names the older daemons when every Server answers", async () => {
    const read = await reading({ machines: [machine("new", "0.2.5"), machine("old", "0.2.4")], incomplete_ids: { machines: [] } });
    expect(read).toMatchObject({ _tag: "Success", success: ["old"] });
  });

  it("fails while a Server is missing from the frame, since it may run an older daemon", async () => {
    const read = await reading({ machines: [machine("new", "0.2.5")], incomplete_ids: { machines: ["silent"] } });
    expect(read).toMatchObject({ _tag: "Failure" });
  });
});
