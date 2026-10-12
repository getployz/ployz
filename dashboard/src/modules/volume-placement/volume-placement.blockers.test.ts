import { Effect } from "effect";
import { describe, expect, it } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { requirements } from "#/modules/runtime/daemon-compat";
import { type ConnectedRuntimeClient, OrganizationRuntime, type OrganizationRuntimeService } from "#/modules/runtime/organization-runtime.server";
import { readPlacementBlockers } from "./volume-placement.deploy.server";

const machine = (name: string, servesVolumeMove: boolean | "unknown", daemonVersion = "0.2.5") => ({
  machine: { name, runtime: {
    daemon_version: servesVolumeMove === "unknown" ? "dev" : daemonVersion,
    capabilities: servesVolumeMove === "unknown" ? [] : servesVolumeMove ? [requirements.volumeMove.capability] : ["ployz.logs.tail.v1"],
  } },
  storage: { state: "pool" },
});

const released = (name: string, daemonVersion: string) => ({ machine: { name, runtime: { daemon_version: daemonVersion } }, storage: { state: "pool" } });

type Frame = { readonly machines: (ReturnType<typeof machine> | ReturnType<typeof released>)[]; readonly incomplete_ids: { readonly machines: string[] } };

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
    const read = await reading({ machines: [machine("new", true), machine("old", false)], incomplete_ids: { machines: [] } });
    expect(read).toMatchObject({ _tag: "Success", success: ["old"] });
  });

  it("names released 0.2.3 and 0.2.4 daemons, which list no capabilities, as lacking volume move", async () => {
    const read = await reading({ machines: [machine("new", true), released("old-3", "0.2.3"), released("old-4", "0.2.4")], incomplete_ids: { machines: [] } });
    expect(read).toMatchObject({ _tag: "Success", success: ["old-3", "old-4"] });
  });

  it("fails as unknown, not lacking, when a Server's record lists no capabilities and no readable version", async () => {
    const read = await reading({ machines: [machine("new", true), machine("quiet", "unknown")], incomplete_ids: { machines: [] } });
    expect(read).toMatchObject({ _tag: "Failure", failure: { _tag: "SupportUnknown", servers: ["quiet"] } });
  });

  it("names only the confirmed lacks when lacking and unknown Servers both hold placement", async () => {
    const read = await reading({ machines: [machine("old", false), machine("quiet", "unknown")], incomplete_ids: { machines: [] } });
    expect(read).toMatchObject({ _tag: "Success", success: ["old"] });
  });

  it("fails while a Server is missing from the frame, since it may lack volume move", async () => {
    const read = await reading({ machines: [machine("new", true)], incomplete_ids: { machines: ["silent"] } });
    expect(read).toMatchObject({ _tag: "Failure" });
  });
});
