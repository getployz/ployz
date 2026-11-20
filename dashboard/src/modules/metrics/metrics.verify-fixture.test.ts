import { describe, expect, it } from "vitest";
import { hostHistory, serviceHistory } from "./metrics-history";
import { serverMetrics, serviceMetrics } from "./metrics-model";
import { FOURS, NORMAL, verifyServerData, verifyServiceData } from "./metrics.verify-fixture";
import { stepFor } from "./metrics-series";

const window = { from: 1_700_000_000_000, to: 1_700_003_600_000 };
const range = { window, stepMs: stepFor(window) };
const reading = { ...range, at: window.to };

const service = (name: string) => {
  const { rows, crashes } = verifyServiceData(name, range);
  return serviceMetrics({ type: "history", window, at: window.to, history: serviceHistory(rows, crashes, reading) }, new Map());
};
const server = (id: string) => {
  const { rows, disk } = verifyServerData(id, range);
  return serverMetrics({ type: "history", window, at: window.to, history: hostHistory(rows, disk, reading) });
};

describe("the verify-dashboard fixture, read as ClickHouse rows", () => {
  it("draws the seeded Services: web on two Servers, postgres with a gap, IO and both crash kinds", () => {
    const web = service("web");
    expect(web.availability).toEqual({ type: "normal" });
    expect(web.byServer).toHaveLength(2);
    expect(web.all.cpu.length).toBeGreaterThan(0);
    expect(service("redis").availability).toEqual({ type: "no_readings" });

    const postgres = service("postgres");
    expect(postgres.allGaps).toHaveLength(1);
    expect(postgres.all.diskRead.length).toBeGreaterThan(0);
    expect(postgres.markers.map((marker) => marker.kind)).toEqual(["oom", "exit"]);
  });

  it("draws the seeded Servers: one with a gap and every disk part, one silent", () => {
    const normal = server(NORMAL);
    expect(normal.availability).toEqual({ type: "normal" });
    expect(normal.interfaces.map((entry) => entry.name)).toEqual(["eth0"]);
    expect(normal.gaps).toHaveLength(1);
    expect(normal.disk[0]).toMatchObject({ volumes: expect.any(Number), mirrors: expect.any(Number), logsAndMetrics: expect.any(Number), other: expect.any(Number) });
    expect(server(FOURS).availability).toEqual({ type: "no_readings" });
    expect(verifyServerData("f".repeat(32), range).reporting).toBe(false);
  });
});
