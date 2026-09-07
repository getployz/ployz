import { PgDialect, getTableConfig } from "drizzle-orm/pg-core";
import { describe, expect, it } from "vitest";
import { asString } from "#/lib/json";
import { destructiveVolumeAttempt } from "#/db/schema";

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

describe("destructive volume attempt database invariants", () => {
  it("allows only one active attempt for a deployed volume", () => {
    const active = getTableConfig(destructiveVolumeAttempt).indexes.find(
      ({ config }) =>
        config.name === "destructive_volume_attempt_one_active_target_idx",
    );

    expect(active?.config.unique).toBe(true);
    expect(active?.config.columns.map(columnName)).toEqual([
      "environment_resource_id",
    ]);
    expect(active?.config.where).toBeDefined();
  });

  it("retains deployment and retry provenance", () => {
    const config = getTableConfig(destructiveVolumeAttempt);
    const deploymentForeignKey = config.foreignKeys.find(({ reference }) =>
      reference().columns.some(
        ({ name }) => name === "environment_deployment_id",
      ),
    );
    const retryForeignKey = config.foreignKeys.find(({ reference }) =>
      reference().columns.some(({ name }) => name === "retry_of_attempt_id"),
    );
    const retryIndex = config.indexes.find(
      ({ config: index }) =>
        index.name === "destructive_volume_attempt_retry_of_idx",
    );

    expect(deploymentForeignKey?.onDelete).toBe("restrict");
    expect(retryForeignKey?.onDelete).toBe("restrict");
    expect(retryIndex?.config.unique).toBe(true);
  });

  it("requires operation evidence for accepted and terminal dispositions", () => {
    const checks = getTableConfig(destructiveVolumeAttempt).checks;
    const evidence = checks.find(
      ({ name }) => name === "destructive_volume_attempt_evidence_check",
    );
    expect(evidence).toBeDefined();
    if (!evidence) return;

    const rendered = new PgDialect().sqlToQuery(evidence.value).sql;
    expect(rendered).toContain("'accepted'");
    expect(rendered).toContain("'completed'");
    expect(rendered).toContain("'partial'");
    expect(rendered).toContain("'core_terminal'");
    expect(rendered).toContain("'cloud_timeout'");
    expect(rendered).toContain("'cloud_cancelled'");
  });
});
