import { getTableConfig, PgDialect } from "drizzle-orm/pg-core";
import { describe, expect, it } from "vitest";
import {
  coreOperationWatch,
  type StoredCoreOperationExpectedKind,
} from "#/db/schema";

describe("core operation watch database invariants", () => {
  it("stores optional immutable observation detail for terminal close replay", () => {
    const detail = getTableConfig(coreOperationWatch).columns.find(
      ({ name }) => name === "observation_detail",
    );
    expect(detail?.notNull).toBe(false);
    expect(detail?.dataType).toBe("object json");

  });

  it("admits every generated alpha.66 kind and preserves retired watch evidence", () => {
    const check = getTableConfig(coreOperationWatch).checks.find(
      ({ name }) => name === "core_operation_watch_expected_kind_check",
    );
    expect(check).toBeDefined();
    if (!check) return;

    const sql = new PgDialect().sqlToQuery(check.value).sql;
    for (const kind of [
      "deploy",
      "cert",
      "machine_add",
      "machine_update",
      "machine_lifecycle",
      "core_replace",
      "credential_grant",
      "network_repair",
      "service_restart",
      "managed_dns_reconcile",
      "ingress_configure",
      "namespace_remove",
      "volume_create",
      "volume_remove",
    ]) {
      expect(sql).toContain(`'${kind}'`);
    }
    expect(sql).toContain("'ingress_refresh'");

    const existingIngressRefreshWatch: StoredCoreOperationExpectedKind =
      "ingress_refresh";
    expect(existingIngressRefreshWatch).toBe("ingress_refresh");
  });

});
