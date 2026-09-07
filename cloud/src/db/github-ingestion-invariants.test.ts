import { getTableConfig } from "drizzle-orm/pg-core";
import { describe, expect, it } from "vitest";
import { githubWebhookDelivery } from "#/db/schema";

describe("GitHub ingestion schema invariants", () => {
  it("uniquely indexes non-null processing run ownership", () => {
    const index = getTableConfig(githubWebhookDelivery).indexes.find(
      ({ config }) =>
        config.name === "github_webhook_delivery_processing_run_id_idx",
    );

    expect(index?.config.unique).toBe(true);
    expect(index?.config.where).toBeDefined();
    expect(index?.config.columns).toHaveLength(1);
    expect(index?.config.columns[0]).toMatchObject({
      name: "processing_run_id",
    });
  });
});
