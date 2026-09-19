import { expect, it } from "vitest";
import { compareDashboardServiceSettings } from "./config-changes";
import { parseDashboardServiceConfig } from "./service-config";

const template = (lineageId: string) => parseDashboardServiceConfig({
  version: 2, name: "API", privateDns: "api",
  source: { version: 1, type: "empty", rootDir: "/" },
  healthcheck: { type: "none" }, restartPolicy: "unless-stopped",
  env: { URL: { kind: "literal", value: "${{ shared.HOST }}/${{ api.PORT }}", parts: [
    { kind: "ref", owner: { scope: "variable_group", lineageId: "00000000-0000-4000-8000-000000000001" }, key: "HOST" },
    { kind: "text", value: "/" },
    { kind: "ref", owner: { scope: "service", lineageId }, key: "PORT" },
  ] } },
});

it("reviews a repointed Service reference in a mixed template with unchanged display text", () => {
  const before = template("00000000-0000-4000-8000-000000000002");
  const after = template("00000000-0000-4000-8000-000000000003");
  expect(compareDashboardServiceSettings(after, before)).toMatchObject([
    { path: "env.URL", kind: "update", canRestore: false },
  ]);
  expect(compareDashboardServiceSettings(before, before)).toEqual([]);
  expect(after.env).toEqual(template("00000000-0000-4000-8000-000000000003").env);
});
