import { afterAll, beforeAll, beforeEach, describe, expect, it } from "vitest";
import { desc, eq } from "drizzle-orm";
import { Effect } from "effect";
import * as schema from "#/db/schema";
import { decodeStrict } from "./schema";
import { savedEnvironmentIntentSchema } from "./saved-intent";
import {
  discardEnvironmentSavedState,
  publishEnvironmentSavedState,
} from "./saved-state-operations.server";
import type { EnvironmentSavedStateBasis } from "./saved-state";
import {
  type GithubPostgresTestHarness,
  startGithubPostgresTestHarness,
} from "#/modules/github/github-ingestion.postgres-test-harness";
import {
  makeSecretEncryption,
  SecretEncryption,
} from "#/utils/encrypted-secret.server";

const encryption = makeSecretEncryption("test-encryption-secret");

const organizationId = "00000000-0000-4000-8000-000000000301";
const userId = "00000000-0000-4000-8000-000000000302";
const projectId = "00000000-0000-4000-8000-000000000303";
const environmentId = "00000000-0000-4000-8000-000000000304";
const firstVolumeId = "00000000-0000-4000-8000-000000000305";
const firstLineageId = "00000000-0000-4000-8000-000000000306";
const secondVolumeId = "00000000-0000-4000-8000-000000000307";
const secondLineageId = "00000000-0000-4000-8000-000000000308";

const emptyIntent = {
  version: 1 as const,
  environmentSlug: "production",
  services: [],
  variableGroups: [],
  volumes: [],
};

function volumeIntent() {
  return {
    ...emptyIntent,
    volumes: [
      {
        resourceId: firstVolumeId,
        resourceLineageId: firstLineageId,
        name: "First",
      },
      {
        resourceId: secondVolumeId,
        resourceLineageId: secondLineageId,
        name: "Second",
      },
    ],
  };
}

function publication(input: {
  basis: EnvironmentSavedStateBasis;
  intent?: typeof emptyIntent | ReturnType<typeof volumeIntent>;
  destructiveVolumeReviews?: Parameters<
    typeof publishEnvironmentSavedState
  >[0]["destructiveVolumeReviews"];
}) {
  return {
    environmentId,
    actorId: userId,
    message: null,
    basis: input.basis,
    intent: input.intent ?? emptyIntent,
    destructiveVolumeReviews: input.destructiveVolumeReviews ?? [],
    revisionPolicy: "always_create" as const,
  };
}

describe("Environment Saved State aggregate", () => {
  let harness: GithubPostgresTestHarness;

  beforeAll(async () => {
    harness = await startGithubPostgresTestHarness();
  }, 60_000);

  afterAll(async () => {
    await harness?.stop();
  });

  beforeEach(async () => {
    await harness.pool.query(`
      truncate table environment_saved_state_snapshot, environment, project,
        "user", organization cascade;
      insert into organization (id, name, slug)
      values ('${organizationId}', 'Acceptance', 'acceptance');
      insert into "user" (id, email, name)
      values ('${userId}', 'owner@example.com', 'Owner');
      insert into project (id, organization_id, name, slug)
      values ('${projectId}', '${organizationId}', 'Cloud', 'cloud');
      insert into environment (
        id, project_id, organization_id, name, namespace
      ) values (
        '${environmentId}', '${projectId}', '${organizationId}',
        'Production', 'production'
      );
    `);
  });

  it("serializes simultaneous publishers so one exact basis wins", async () => {
    const attempted = await Promise.allSettled(
      ["first", "second"].map((message) =>
        harness.runTransaction(() =>
            publishEnvironmentSavedState(
              { ...publication({ basis: { kind: "no_saved_state" } }), message }
            ).pipe(Effect.provideService(SecretEncryption, encryption)),
        ),
      ),
    );

    expect(attempted.filter(({ status }) => status === "fulfilled")).toHaveLength(
      1,
    );
    const rejected = attempted.find(({ status }) => status === "rejected");
    expect(rejected).toMatchObject({
      status: "rejected",
      reason: { _tag: "Conflict" },
    });
    expect(
      await harness.db.select().from(schema.environmentSavedStateSnapshot),
    ).toHaveLength(1);
  });

  it("carries destructive Volume authority and revokes it when restored", async () => {
    const review = {
      target: {
        version: 1 as const,
        resourceId: firstVolumeId,
        namespaceId: "production",
        volumeName: `vol-${firstVolumeId}`,
        machineId: "machine-1",
      },
      evidence: {
        version: 1 as const,
        fingerprint: "reviewed-volume-removal",
        reviewedAt: "2026-09-04T00:00:00.000Z",
        evidence: {
          namespaceId: "production",
          volumeName: `vol-${firstVolumeId}`,
          machineId: "machine-1",
          kind: { kind: "plain" as const },
          availability: {
            status: "available" as const,
            usedBytes: 0,
            lastWriteUnixSeconds: 0,
          },
          referencingServices: [],
        },
      },
    };
    const first = await harness.runTransaction(() =>
        publishEnvironmentSavedState(
          publication({
            basis: { kind: "no_saved_state" },
            destructiveVolumeReviews: [review],
          })
        ).pipe(Effect.provideService(SecretEncryption, encryption)),
    );
    const carried = await harness.runTransaction(() =>
        publishEnvironmentSavedState(
          publication({
            basis: {
              kind: "saved_revision",
              savedStateSnapshotId: first.savedStateSnapshotId,
            },
          })
        ).pipe(Effect.provideService(SecretEncryption, encryption)),
    );
    const restoredVolume = volumeIntent().volumes[0];
    if (restoredVolume === undefined) throw new Error("missing Volume fixture");
    const restored = await harness.runTransaction(() =>
        publishEnvironmentSavedState(
          publication({
            basis: {
              kind: "saved_revision",
              savedStateSnapshotId: carried.savedStateSnapshotId,
            },
            intent: {
              ...emptyIntent,
              volumes: [restoredVolume],
            },
          })
        ).pipe(Effect.provideService(SecretEncryption, encryption)),
    );

    expect(carried.volumeDeletionAuthorizations).toEqual([review]);
    expect(restored.volumeDeletionAuthorizations).toEqual([]);
  });

  it("publishes one replacement revision for an atomic multi-node discard", async () => {
    const initial = await harness.runTransaction(() =>
        publishEnvironmentSavedState(
          publication({
            basis: { kind: "no_saved_state" },
            intent: volumeIntent(),
          })
        ).pipe(Effect.provideService(SecretEncryption, encryption)),
    );
    const discarded = await harness.runTransaction(() =>
        discardEnvironmentSavedState(
          {
            environmentId,
            actorId: userId,
            command: {
              kind: "discard",
              basis: {
                kind: "saved_revision",
                savedStateSnapshotId: initial.savedStateSnapshotId,
              },
              operations: [
                { kind: "node", nodeType: "volume", nodeId: firstVolumeId },
                { kind: "node", nodeType: "volume", nodeId: secondVolumeId },
              ],
            },
          }
        ).pipe(Effect.provideService(SecretEncryption, encryption)),
    );
    const rows = await harness.db
      .select({ id: schema.environmentSavedStateSnapshot.id, intent: schema.environmentSavedStateSnapshot.intent })
      .from(schema.environmentSavedStateSnapshot)
      .where(eq(schema.environmentSavedStateSnapshot.environmentId, environmentId))
      .orderBy(desc(schema.environmentSavedStateSnapshot.createdAt));

    expect(rows).toHaveLength(2);
    expect(rows[0]?.id).toBe(discarded.savedStateSnapshotId);
    expect(
      decodeStrict(savedEnvironmentIntentSchema, rows[0]?.intent).volumes,
    ).toEqual([]);
  });
});
