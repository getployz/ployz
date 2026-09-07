import { getTableConfig, PgDialect } from "drizzle-orm/pg-core";
import { describe, expect, it } from "vitest";
import { asString } from "#/lib/json";
import { volumeRemoveAttempt } from "#/db/schema";

function hasColumnName<T>(column: T): column is T & { name: string } {
  return (
    column !== null &&
    column !== undefined &&
    typeof column === "object" &&
    "name" in column &&
    typeof column.name === "string"
  );
}

function columnName<T>(column: T) {
  return hasColumnName(column) ? column.name : asString(column);
}

describe("volume remove attempt database invariants", () => {
  it("allows only one active attempt per Cloud volume resource", () => {
    const active = getTableConfig(volumeRemoveAttempt).indexes.find(
      ({ config }) =>
        config.name === "volume_remove_attempt_one_active_resource_idx",
    );

    expect(active?.config.unique).toBe(true);
    expect(active?.config.columns.map(columnName)).toEqual([
      "environment_resource_id",
    ]);
    expect(active?.config.where).toBeDefined();
  });

  it("stores a unique Inngest run id and retry provenance", () => {
    const config = getTableConfig(volumeRemoveAttempt);
    const runIndex = config.indexes.find(
      ({ config: index }) =>
        index.name === "volume_remove_attempt_inngest_run_uidx",
    );
    const retryIndex = config.indexes.find(
      ({ config: index }) => index.name === "volume_remove_attempt_retry_of_idx",
    );
    const retryForeignKey = config.foreignKeys.find(({ reference }) =>
      reference().columns.some(({ name }) => name === "retry_of_attempt_id"),
    );

    expect(runIndex?.config.unique).toBe(true);
    expect(retryIndex?.config.unique).toBe(true);
    expect(retryForeignKey?.onDelete).toBe("restrict");
  });

  it("does not foreign-key the volume resource so completed jobs can hard-delete the tombstone", () => {
    const resourceForeignKey = getTableConfig(
      volumeRemoveAttempt,
    ).foreignKeys.find(({ reference }) =>
      reference().columns.some(
        ({ name }) => name === "environment_resource_id",
      ),
    );
    expect(resourceForeignKey).toBeUndefined();
  });

  it("requires run ownership for running and terminal statuses", () => {
    const statusCheck = getTableConfig(volumeRemoveAttempt).checks.find(
      ({ name }) => name === "volume_remove_attempt_status_shape_check",
    );
    expect(statusCheck).toBeDefined();
    if (!statusCheck) return;
    const rendered = new PgDialect().sqlToQuery(statusCheck.value).sql;
    expect(rendered).toContain("'pending'");
    expect(rendered).toContain("'running'");
    expect(rendered).toContain("'completed'");
    expect(rendered).toContain("'partial'");
    expect(rendered).toContain("'failed'");
    expect(rendered).toContain("'cancelled'");
  });
});
