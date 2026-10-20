import { expect, it } from "vitest";
import { SERVER_SILENT_AFTER_MS, type ServerContact, serversOffline } from "./servers-offline";

const NOW = Date.parse("2026-10-20T12:00:00Z");
const ago = (ms: number) => new Date(NOW - ms);
const MINUTE = 60_000;
const DAY = 24 * 60 * MINUTE;

const server = (fields: Partial<ServerContact> = {}): ServerContact => ({
  enrolledAt: ago(30 * DAY), pushTarget: "https://cloud.example.com/api/machines/m/observe", lastPushAt: ago(MINUTE), ...fields,
});
const stale = server({ lastPushAt: ago(SERVER_SILENT_AFTER_MS + MINUTE) });

it("counts the Servers offline only once every one has missed three hourly refreshes", () => {
  expect(serversOffline([stale, stale], NOW)).toBe(true);
  expect(serversOffline([stale, server()], NOW)).toBe(false);
  expect(serversOffline([server({ lastPushAt: ago(SERVER_SILENT_AFTER_MS - MINUTE) })], NOW)).toBe(false);
});

it("counts a Server that never pushed from its enrollment", () => {
  expect(serversOffline([server({ lastPushAt: null })], NOW)).toBe(true);
  expect(serversOffline([server({ lastPushAt: null, enrolledAt: ago(10 * MINUTE) })], NOW)).toBe(false);
});

it("takes no silence as evidence from a Server Cloud hasn't given its push URL", () => {
  expect(serversOffline([server({ pushTarget: null, lastPushAt: null })], NOW)).toBe(false);
  expect(serversOffline([stale, server({ pushTarget: null, lastPushAt: null })], NOW)).toBe(false);
});

it("says nothing for an Organization with no Servers", () => {
  expect(serversOffline([], NOW)).toBe(false);
});
