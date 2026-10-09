import { createCollection, localOnlyCollectionOptions } from "@tanstack/react-db";
import { expect, it } from "vitest";
import { appendContainerLogs, boundContainerLogs, historyStart, mergeContainerHistory, projectLogExit, trimContainerLogs, type ContainerLogLine, type ContainerLogRow } from "./container-log.collection";

const row = (containerId: string, timestamp: string, ordinal = 0, origin = "live"): ContainerLogLine => ({
  kind: "line", id: `${origin}/server/${containerId}/${timestamp}/${ordinal}`, timestamp,
  machineId: "server", machineName: "Server", containerId, serviceName: "api", channel: "stdout", level: "info", message: "same message",
});

it("the Log Store's read of a timestamp group replaces the live tail's, and a replayed tail stays out", async () => {
  const collection = createCollection(localOnlyCollectionOptions({ id: "log-test", getKey: (row: ContainerLogRow) => row.id }));
  await collection.preload();
  const stored = new Set<string>();
  // The tail began inside the group at 100: it has one of its two identical lines.
  appendContainerLogs(collection, [row("busy", "100", 1), row("busy", "110"), row("quiet", "10")], stored);
  mergeContainerHistory(collection, [row("busy", "99", 0, "store"), row("busy", "100", 0, "store"), row("busy", "100", 1, "store")], stored);
  expect([...collection.values()].map(kept => kept.id).sort()).toEqual([
    "live/server/busy/110/0", "live/server/quiet/10/0", "store/server/busy/100/0", "store/server/busy/100/1", "store/server/busy/99/0",
  ]);
  appendContainerLogs(collection, [row("busy", "100", 1), row("busy", "110"), row("busy", "120")], stored);
  expect(collection.size).toBe(6);
  expect(collection.has("live/server/busy/120/0")).toBe(true);
  // A group split across two Store pages keeps both halves.
  mergeContainerHistory(collection, [row("busy", "100", 2, "store")], stored);
  expect(collection.has("store/server/busy/100/0")).toBe(true);
  expect(collection.has("store/server/busy/100/2")).toBe(true);
  await collection.cleanup();
});

it("keeps only the newest lines past the limit, and a batch with repeats lands once", async () => {
  const collection = createCollection(localOnlyCollectionOptions({ id: "log-trim-test", getKey: (row: ContainerLogRow) => row.id }));
  await collection.preload();
  appendContainerLogs(collection, [row("a", "30"), row("a", "10"), row("a", "20"), row("a", "20")]);
  expect(collection.size).toBe(3);
  trimContainerLogs(collection, 2);
  expect([...collection.values()].map(kept => kept.timestamp).sort()).toEqual(["20", "30"]);
  await collection.cleanup();
});

it("the Log Store starts after the lines every container already has", () => {
  expect(historyStart([])).toBeUndefined();
  // busy's tail reaches back to 90, a restarted container's only to 100; from 100 on, both are loaded.
  expect(historyStart([row("busy", "90"), row("busy", "120"), row("fresh", "100"), row("fresh", "130")])).toBe("100");
  expect(historyStart([row("busy", "90"), { ...row("busy", "5"), channel: "lifecycle" }])).toBe("90");
});

it("scrollback the viewer loaded outlasts a flood of live lines until the page holds twice the limit", async () => {
  const collection = createCollection(localOnlyCollectionOptions({ id: "log-bound-test", getKey: (row: ContainerLogRow) => row.id }));
  await collection.preload();
  appendContainerLogs(collection, [row("a", "1", 0, "store"), row("a", "2", 0, "store"), ...["10", "11", "12"].map(at => row("a", at))]);
  expect(boundContainerLogs(collection, "10", 3)).toBe(false);
  appendContainerLogs(collection, [row("a", "13")]);
  expect(boundContainerLogs(collection, "10", 3)).toBe(false);
  expect(collection.size).toBe(6);
  appendContainerLogs(collection, [row("a", "14")]);
  expect(boundContainerLogs(collection, "10", 3)).toBe(true);
  expect([...collection.values()].map(kept => kept.timestamp).sort()).toEqual(["12", "13", "14"]);
  // Nothing scrolled back: live lines trim from the oldest.
  appendContainerLogs(collection, [row("a", "15"), row("a", "16")]);
  expect(boundContainerLogs(collection, undefined, 3)).toBe(false);
  expect([...collection.values()].map(kept => kept.timestamp).sort()).toEqual(["14", "15", "16"]);
  await collection.cleanup();
});

it("a container's exit reads as a lifecycle line, an error unless it exited cleanly", () => {
  const exit = { machineId: "server", machineName: "Server", containerId: "a", serviceName: "api", timestampNanos: "5" };
  expect([
    projectLogExit({ ...exit, exitCode: 137, oomKilled: true }),
    projectLogExit({ ...exit, exitCode: 1, oomKilled: false }),
    projectLogExit({ ...exit, exitCode: 0, oomKilled: false }),
  ].map(line => [line.channel, line.level, line.message])).toEqual([
    ["lifecycle", "error", "Out of memory (exit code 137)"], ["lifecycle", "error", "Exited with code 1"], ["lifecycle", "info", "Exited with code 0"],
  ]);
});
