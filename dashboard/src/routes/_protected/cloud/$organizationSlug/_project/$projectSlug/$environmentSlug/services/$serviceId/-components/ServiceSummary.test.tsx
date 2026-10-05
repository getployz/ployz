// @vitest-environment jsdom
import type { JsonValue, ServiceSettingChange, SettingRow } from "@ployz/sdk";
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { deployedHealthcheck, HealthcheckWarning } from "./ServiceSummary";

const row = (value: JsonValue): SettingRow => ({ path: "db.healthcheck", value, default: null, apply: "staged" });
const update = (before: JsonValue, after: JsonValue): ServiceSettingChange =>
  ({ path: "db.healthcheck", kind: "update", before, after, canRestore: true, row: null });

describe("HealthcheckWarning", () => {
  afterEach(cleanup);

  it("says the healthcheck is failing, with the deployed command under it", () => {
    const check = { command: "pg_isready -h 127.0.0.1 -p 5432", timeoutSeconds: 300 };
    render(<HealthcheckWarning check={deployedHealthcheck({ rows: new Map([["healthcheck", row(check)]]), changes: new Map() })} />);
    expect(screen.getByRole("note").textContent).toBe("Healthcheck failingpg_isready -h 127.0.0.1 -p 5432");
  });

  it("shows the deployed check, not the one staged for the next Deploy", () => {
    const staged = { command: "redis-cli ping", timeoutSeconds: 300 };
    render(<HealthcheckWarning check={deployedHealthcheck({ rows: new Map([["healthcheck", row(staged)]]),
      changes: new Map([["healthcheck", update({ path: "/up", timeoutSeconds: 30 }, staged)]]) })} />);
    expect(screen.getByRole("note").textContent).toBe("Healthcheck failing/up");
  });

  it("names no check when none is deployed, even with one staged", () => {
    const staged = { path: "/up", timeoutSeconds: 30 };
    render(<HealthcheckWarning check={deployedHealthcheck({ rows: new Map([["healthcheck", row(staged)]]),
      changes: new Map([["healthcheck", update(null, staged)]]) })} />);
    expect(screen.getByRole("note").textContent).toBe("Healthcheck failing");
  });
});
