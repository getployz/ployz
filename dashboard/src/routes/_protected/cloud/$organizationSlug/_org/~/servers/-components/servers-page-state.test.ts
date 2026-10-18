import { expect, test } from "vitest";
import { serversPageState } from "./servers-page-state";

test("zero servers with nothing waiting is first run without the intake tray", () => {
  expect(serversPageState({ list: "live", servers: 0, waiting: 0, enrolling: 0 })).toEqual({ kind: "first-run", intake: false });
});

test("a server arriving on first run opens the intake tray", () => {
  expect(serversPageState({ list: "live", servers: 0, waiting: 1, enrolling: 0 })).toEqual({ kind: "first-run", intake: true });
  expect(serversPageState({ list: "live", servers: 0, waiting: 0, enrolling: 1 })).toEqual({ kind: "first-run", intake: true });
});

test("servers make a fleet, and the tray shows only with something in it", () => {
  expect(serversPageState({ list: "live", servers: 3, waiting: 0, enrolling: 0 })).toEqual({ kind: "fleet", intake: false, stale: false });
  expect(serversPageState({ list: "stale", servers: 3, waiting: 2, enrolling: 0 })).toEqual({ kind: "fleet", intake: true, stale: true });
});

test("loading and unreachable keep the tray, which comes from Cloud", () => {
  expect(serversPageState({ list: "loading", servers: 0, waiting: 0, enrolling: 0 })).toEqual({ kind: "loading", intake: false });
  expect(serversPageState({ list: "unreachable", servers: 0, waiting: 4, enrolling: 0 })).toEqual({ kind: "unreachable", intake: true });
});
