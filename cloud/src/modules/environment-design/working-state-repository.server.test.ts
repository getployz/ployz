import { describe, expect, it } from "vitest";
import { Effect } from "effect";
import { asTestDouble } from "#/lib/test-double";
import { Database, type DatabaseService } from "#/server/database.server";
import { loadCurrentEnvironmentSnapshotProjection } from "#/modules/environment-design/working-state-repository.server";
import {
  createDefaultServiceHealthcheck,
  createDefaultServiceRestartPolicy,
  createImageServiceSource,
} from "#/modules/environment-design/services";

function queryResult(rows: unknown[]) {
  const query = Object.assign(Effect.succeed(rows), {
    from: () => query,
    innerJoin: () => query,
    leftJoin: () => query,
    where: () => query,
    orderBy: () => query,
  });
  return query;
}

describe("current environment deployment snapshot", () => {
  it("captures the edited managed hostname in the service config", async () => {
    const serviceId = "11111111-1111-4111-8111-111111111111";
    const lineageId = "22222222-2222-4222-8222-222222222222";
    const selections = [
      [{ slug: "production" }],
      [
        {
          id: serviceId,
          environmentId: "33333333-3333-4333-8333-333333333333",
          lineageId,
          slug: "api",
          name: "API",
          environmentSlug: "production",
          source: createImageServiceSource({ image: "ghcr.io/acme/api:sha" }),
          preDeployCommand: null,
          startCommand: null,
          healthcheck: createDefaultServiceHealthcheck(),
          restartPolicy: createDefaultServiceRestartPolicy(),
          maxRetries: 10,
          cron: null,
          replicas: 1,
          cpuLimit: null,
          memLimit: null,
          privateDns: "api",
          routes: [],
          managedHostname: { prefix: "public-api", targetPort: 4000 },
          build: { builder: "auto", dockerfilePath: null, watchPaths: [] },
          updatedAt: new Date("2026-08-11T20:00:00.000Z"),
          encryptedRegistryUsername: null,
          encryptedRegistrySecret: null,
        },
      ],
      [],
      [],
      [],
      [],
    ];
    const database = asTestDouble<DatabaseService>()({
      drizzle: asTestDouble<DatabaseService["drizzle"]>()({
        select: () => queryResult(selections.shift() ?? []),
      }),
    });

    const projection = await Effect.runPromise(
      loadCurrentEnvironmentSnapshotProjection(
        "33333333-3333-4333-8333-333333333333",
      ).pipe(Effect.provideService(Database, database)),
    );

    expect(projection.nodeSnapshots).toContainEqual(
      expect.objectContaining({
        nodeType: "service",
        nodeId: serviceId,
        nodeLineageId: lineageId,
        config: expect.objectContaining({
          managedHostname: { prefix: "public-api", targetPort: 4000 },
        }),
      }),
    );
  });
});
