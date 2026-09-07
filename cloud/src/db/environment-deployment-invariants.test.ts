import { describe, expect, it } from "vitest";
import { getTableConfig } from "drizzle-orm/pg-core";
import { environmentDeployment } from "#/db/schema";

describe("environment deployment database invariants", () => {
  it("keeps a normal environment lookup index", () => {
    const table = getTableConfig(environmentDeployment);
    const index = table.indexes.find(
      (candidate) =>
        candidate.config.name === "environment_deployment_environment_id_idx",
    );

    expect(index?.config.unique).toBe(false);
    expect(index?.config.where).toBeUndefined();
    expect(index?.config.columns).toHaveLength(1);
    const column = index?.config.columns[0];
    expect(column && "name" in column ? column.name : undefined).toBe(
      "environment_id",
    );
  });

  it("admits one queued target and one started attempt per environment", () => {
    const table = getTableConfig(environmentDeployment);
    const queuedIndex = table.indexes.find(
      (candidate) =>
        candidate.config.name ===
        "environment_deployment_one_queued_target_idx",
    );
    const startedIndex = table.indexes.find(
      (candidate) =>
        candidate.config.name ===
        "environment_deployment_one_started_attempt_idx",
    );

    for (const index of [queuedIndex, startedIndex]) {
    expect(index?.config.unique).toBe(true);
    expect(index?.config.columns).toHaveLength(1);
    const column = index?.config.columns[0];
    expect(column && "name" in column ? column.name : undefined).toBe(
      "environment_id",
    );
    expect(index?.config.where).toBeDefined();
    }
  });
});
