import { eq } from "drizzle-orm";
import { Database } from "#/server/database.server";
import { loadCurrentEnvironmentState } from "./working-state-repository.server";
import { fingerprintReviewedEnvironmentWorkingStateSync } from "./working-state-fingerprint.server";
import { afterAll, beforeAll, beforeEach, describe, expect, it } from "vitest";
import { Effect } from "effect";
import * as schema from "#/db/schema";
import {
  saveReviewedEnvironmentState,
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
    typeof saveReviewedEnvironmentState
  >[0]["review"]["destructiveVolumeReviews"];
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

function publishEnvironmentSavedState(input: Omit<ReturnType<typeof publication>, "message"> & { message: string | null }) {
  return Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const [document] = yield* drizzle.select().from(schema.environment).where(eq(schema.environment.id, environmentId));
    if (JSON.stringify(document?.intent) !== JSON.stringify(input.intent)) {
      yield* drizzle.update(schema.environment).set({ intent: input.intent }).where(eq(schema.environment.id, environmentId));
    }
    const state = yield* loadCurrentEnvironmentState(environmentId);
    return yield* saveReviewedEnvironmentState({
      environmentId, actorId: userId, message: input.message,
      review: { savedStateBasis: input.basis, workingStateFingerprint: fingerprintReviewedEnvironmentWorkingStateSync(state.projection),
        destructiveServiceIds: [], destructiveVolumeReviews: [...input.destructiveVolumeReviews] },
    });
  });
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
        id, project_id, organization_id, name, namespace, intent
      ) values (
        '${environmentId}', '${projectId}', '${organizationId}',
        'Production', 'production', '{"version":1,"environmentSlug":"production","services":[],"volumes":[]}'
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
    await harness.db.insert(schema.environmentSavedStateSnapshot).values({
      organizationId, environmentId, actorId: userId, intent: { ...emptyIntent, volumes: [volumeIntent().volumes[0]] }, volumeDeletionAuthorizations: [],
    });
    const [baseline] = await harness.db.select().from(schema.environmentSavedStateSnapshot);
    if (!baseline) throw new Error("missing baseline");
    const [applied] = await harness.db.insert(schema.environmentDeployment).values({
      organizationId, environmentId, savedStateSnapshotId: baseline.id, triggerOrigin: { origin: "manual", actorId: userId }, status: "applied", finishedAt: new Date(),
    }).returning();
    if (!applied) throw new Error("missing applied baseline");
    await harness.db.insert(schema.environmentNodeConfigSnapshot).values({
      organizationId, environmentId, environmentDeploymentId: applied.id, nodeType: "volume", nodeId: firstVolumeId, nodeLineageId: firstLineageId,
      configVersion: 2, config: { version: 2, name: "First", storage: { kind: "plain" } },
    });
    const first = await harness.runTransaction(() =>
        publishEnvironmentSavedState(
          publication({
            basis: { kind: "saved_revision", savedStateSnapshotId: baseline.id },
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
            destructiveVolumeReviews: [review],
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


});
