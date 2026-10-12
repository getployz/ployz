import { Effect } from "effect";
import { describe, expect, it } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { requirements } from "#/modules/runtime/daemon-compat";
import { type ConnectedRuntimeClient, OrganizationRuntime, type OrganizationRuntimeService } from "#/modules/runtime/organization-runtime.server";
import { readPlacementBlockers } from "./volume-placement.deploy.server";

const machine = (name: string, capabilities: readonly string[]) => ({ machine: { name, runtime: { capabilities } }, storage: { state: "pool" } });

const silent = (name: string) => ({ machine: { name, runtime: {} }, storage: { state: "pool" } });

type Frame = { readonly machines: (ReturnType<typeof machine> | ReturnType<typeof silent>)[]; readonly incomplete_ids: { readonly machines: string[] } };

const SERVES = [requirements.volumeMove.capability];
const LACKS = ["ployz.logs.tail.v1"];

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
  it("names the daemons that lack volume move when every Server answers", async () => {
    const read = await reading({ machines: [machine("new", SERVES), machine("old", LACKS)], incomplete_ids: { machines: [] } });
    expect(read).toMatchObject({ _tag: "Success", success: ["old"] });
  });

  it("names a Server whose record lists no capabilities as lacking volume move", async () => {
    const read = await reading({ machines: [machine("new", SERVES), machine("listed-none", []), silent("unlisted")], incomplete_ids: { machines: [] } });
    expect(read).toMatchObject({ _tag: "Success", success: ["listed-none", "unlisted"] });
  });

  it("fails while a Server is missing from the frame, since it may lack volume move", async () => {
    const read = await reading({ machines: [machine("new", SERVES)], incomplete_ids: { machines: ["silent"] } });
    expect(read).toMatchObject({ _tag: "Failure" });
  });
});
