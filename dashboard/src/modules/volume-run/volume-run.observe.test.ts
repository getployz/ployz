import { Effect } from "effect";
import { describe, expect, it } from "vitest";
import type { MachineId } from "@ployz/sdk";
import { asTestDouble } from "#/lib/test-double";
import { type ConnectedRuntimeClient, OrganizationRuntime, type OrganizationRuntimeService } from "#/modules/runtime/organization-runtime.server";
import { Database } from "#/server/database.server";
import { placementChanges } from "#/modules/volume-placement/volume-placement";
import { COPIES } from "./volume-run.test-fixture";
import { observeVolumeMembers } from "./volume-run.server";

const A = "a".padEnd(32, "0") as MachineId;
const B = "b".padEnd(32, "0") as MachineId;

const observing = (incomplete: readonly MachineId[]) => observeVolumeMembers("org-1", "pg-data").pipe(
  Effect.provideService(OrganizationRuntime, asTestDouble<OrganizationRuntimeService>()({
    cancel: () => Effect.void,
    open: () => Effect.succeed({ status: "connected" as const, connected: asTestDouble<ConnectedRuntimeClient>()({
      watchFirstFrame: () => Effect.succeed({
        machines: [{ machine: { id: A, name: "a", public_key: Array.from({ length: 32 }, () => 1), runtime: { daemon_version: "" } }, storage: { state: "pool" } }],
        incomplete_ids: { machines: [...incomplete] },
      }),
      volumeSwitch: () => Effect.succeed({ copy: COPIES.writer }),
    }) }),
  })),
  // Only a Server that lacks the verb reads its release channel from the database.
  Effect.provideService(Database, undefined as never),
  Effect.runPromise,
);

const stagedChanges = (members: Parameters<typeof placementChanges>[1]) => placementChanges({ id: "v1", name: "pg-data" }, members, {
  volumeId: "v1", environmentId: "e1", mirrors: [], staged: { primary: A, mirrors: [B], deletes: [] }, unreached: false, deploymentId: null,
});

describe("observeVolumeMembers", () => {
  it("keeps a staged mirror on a Server the frame has not filled in yet", async () => {
    expect(stagedChanges(await observing([B]))).toMatchObject([{ kind: "add_mirror", server: { id: B } }]);
  });

  it("drops a staged mirror on a Server the cluster no longer lists", async () => {
    expect(stagedChanges(await observing([]))).toEqual([]);
  });
});
