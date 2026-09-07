import { describe, expect, it, vi } from "vitest";
import { Effect } from "effect";
import { asTestDouble } from "#/lib/test-double";
import {
  createDestructiveVolumeActions,
  DestructiveVolumeActionError,
  type DestructiveVolumeActionDeps,
} from "#/modules/operations/destructive-volume-preparation.server";
import type { ConnectedRuntimeClient } from "#/modules/runtime/organization-runtime.server";
import {
  runtimeWatchFrameFixture,
  runtimeWatchVolumeFixture,
} from "#/modules/runtime/runtime-watch-frame.test-fixture";

const namespace = "env-production";

describe("destructive volume preparation", () => {
  it("prepares fresh evidence for Cloud environment volumes in the watch frame", async () => {
    const harness = createHarness(
      [
        { machine_id: "machine-1", name: "vol-resource-1" },
        { machine_id: "machine-1", name: "vol-other" },
      ],
      new Set(["vol-resource-1"]),
    );

    const prepared = await Effect.runPromise(
      harness.actions.prepareNamespace(namespaceInput()),
    );

    expect(prepared.namespaceId).toBe(namespace);
    expect(prepared.evidence.volumes).toHaveLength(1);
    expect(prepared.evidence.volumes[0]?.evidence).toMatchObject({
      volumeName: "vol-resource-1",
      machineId: "machine-1",
      availability: { status: "no_answer" },
      kind: { kind: "plain" },
    });
    expect(harness.close).toHaveBeenCalledOnce();
  });

  it("rejects duplicate authoritative snapshots", async () => {
    const harness = createHarness([
      { machine_id: "machine-1", name: "vol-resource-1" },
      { machine_id: "machine-2", name: "vol-resource-1" },
    ]);

    const result = await Effect.runPromise(
      harness.actions.prepareNamespace(namespaceInput()).pipe(Effect.flip),
    );

    expect(result).toBeInstanceOf(DestructiveVolumeActionError);
    if (result instanceof DestructiveVolumeActionError) {
      expect(result.reason).toBe("volume_duplicate");
    }
  });
});

function namespaceInput() {
  return {
    actor: { userId: "user-1" },
    organizationSlug: "acme",
    environmentId: "environment-1",
  };
}

function createHarness(
  volumes: Array<{ machine_id: string; name: string }>,
  environmentVolumeNames = new Set(["vol-resource-1"]),
) {
  const close = vi.fn(async () => undefined);
  const frame = runtimeWatchFrameFixture({
    volumes: volumes.map((volume) =>
      runtimeWatchVolumeFixture(volume.machine_id, volume.name),
    ),
  });
  const connected = asTestDouble<ConnectedRuntimeClient>()({
    watchFirstFrame: () => Effect.succeed(frame),
  });
  const deps: DestructiveVolumeActionDeps<never, never, never> = {
    authorizeNamespace: vi.fn(() =>
      Effect.succeed({
        organizationId: "organization-1",
        environmentId: "environment-1",
        namespaceId: namespace,
      }),
    ),
    useRuntime: vi.fn((_organizationId, use) =>
      use({ connected }).pipe(Effect.ensuring(Effect.sync(() => close()))),
    ),
    listEnvironmentVolumeNames: vi.fn(() =>
      Effect.succeed(environmentVolumeNames),
    ),
  };
  return {
    actions: createDestructiveVolumeActions(deps),
    close,
  };
}
