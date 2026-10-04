import type { DrainReport, MachineRef, MoveFailure, ServiceDrain } from "@ployz/sdk";
import { describe, expect, it } from "vitest";
import {
  drainButtonLabel,
  drainDialogNames,
  drainRow,
  drainSummary,
  drainView,
  removeHint,
  type LatestDrain,
  type LatestDrains,
} from "#/modules/machines/server-drain";
import { runtimeWatchMachineFixture } from "#/modules/runtime/runtime-watch-frame.test-fixture";

const ref = (digit: string, name: string): MachineRef => {
  const { id } = runtimeWatchMachineFixture(digit.repeat(32), name);
  return { id, name };
};
const web1 = ref("1", "web-1");
const web2 = ref("2", "web-2");
const web3 = ref("3", "web-3");
const server = web2;
const serverNames = new Map([web1, web2, web3].map(({ id, name }) => [id, name]));

const report = (
  services: ServiceDrain[],
  stopped: DrainReport["stopped"] = null,
  remaining: DrainReport["remaining"] = { kind: "observed", services: [] },
): DrainReport => ({
  server: runtimeWatchMachineFixture(web2.id, web2.name),
  services_role: "turned_off",
  services,
  stopped,
  remaining,
});
const finished = (services: ServiceDrain[], stopped: DrainReport["stopped"] = null): LatestDrain => ({
  attemptId: "a1", state: "finished", endedAt: "2026-10-04T10:00:00.000Z", report: report(services, stopped),
});
const view = (latest: LatestDrains, requested: string | null = null) => drainView({ server, latest, serverNames, requested });

const moved: ServiceDrain = { service: "shop/api", result: "moved", moves: [{ from: web2, to: web1 }, { from: web2, to: web3 }, { from: web2, to: web1 }] };
const retired: ServiceDrain = { service: "monitoring/node-exporter", result: "retired" };
const volume: ServiceDrain = { service: "shop/postgres", result: "stays", reason: { kind: "volume", volume: "pg-data", server: web2 } };
const rollout: ServiceDrain = { service: "shop/redis", result: "stays", reason: { kind: "mid_rollout" } };

describe("Drain result", () => {
  it("counts what moved, stopped, stayed and failed, in that order", () => {
    const failed: ServiceDrain = { service: "shop/worker", result: "failed", moves: [], failure: { stage: "no_destination", from: web2, detail: "no room" } };
    expect(drainSummary(report([moved, retired, volume, rollout, failed]), "web-2")).toBe("1 moved, 1 stopped, 2 stayed, 1 failed");
    expect(drainSummary(report([moved, retired]), "web-2")).toBe("Everything moved off web-2");
    expect(drainSummary(report([]), "web-2")).toBe("Nothing was running here");
    expect(drainSummary(report([{ service: "shop/api", result: "nothing_to_move" }]), "web-2")).toBe("Nothing needed to move");
  });

  it("counts what it left alone and never says nothing ran here while something does", () => {
    const left: DrainReport["remaining"] = { kind: "observed", services: ["left-behind/old", "ployz-system/dns", "shop/postgres"] };
    expect(drainSummary(report([], null, left), "web-2")).toBe("2 left alone");
    expect(drainSummary(report([moved, volume], null, left), "web-2")).toBe("1 moved, 1 stayed, 1 left alone");
    expect(drainSummary(report([], null, { kind: "observed", services: ["ployz-system/dns"] }), "web-2")).toBe("Nothing was running here");
  });

  it("says when it couldn't check what's left, and offers Drain again", () => {
    const unobserved = { kind: "unobserved", error: "timeout" } as const;
    expect(drainSummary(report([], null, unobserved), "web-2")).toBe("Couldn't check what's left on web-2");
    expect(drainSummary(report([moved], null, unobserved), "web-2")).toBe("1 moved; couldn't check what's left");
    const own: LatestDrain = { attemptId: "a1", state: "finished", endedAt: "2026-10-04T10:00:00.000Z", report: report([moved], null, unobserved) };
    expect(drainButtonLabel(view({ [web2.id]: own }))).toBe("Drain again");
  });

  it("names each destination once, says why a Service stayed, and stops a Global here", () => {
    expect(drainRow(moved, web2.id)).toMatchObject({ name: "api", namespace: "shop", tone: "moved", label: "Moved to web-1 and web-3", reason: null });
    expect(drainRow(volume, web2.id)).toMatchObject({ tone: "stayed", label: "Stayed", reason: "Its volume pg-data is on this server", pinned: true });
    expect(drainRow(rollout, web2.id)).toMatchObject({ reason: "A deploy is in progress. Deploy it first.", pinned: false });
    expect(drainRow(retired, web2.id)).toMatchObject({ global: true, tone: "stopped", label: "Stopped here", reason: "Runs on every server" });
  });

  it("keeps the moves a failed Service made before it failed, and never claims an unremoved container still serves only here", () => {
    const partial: ServiceDrain = {
      service: "shop/worker",
      result: "failed",
      moves: [{ from: web2, to: web1 }],
      failure: { stage: "not_serving", from: web2, to: web3, detail: "health check timed out", replacement_removed: true },
    };
    expect(drainRow(partial, web2.id)).toMatchObject({
      tone: "failed",
      label: "Failed",
      reason: "Its new container on web-3 didn't become healthy. It still runs here. Moved to web-1 before that.",
    });
    const failure = (failure: MoveFailure): ServiceDrain => ({ service: "shop/worker", result: "failed", moves: [], failure });
    expect(drainRow(failure({ stage: "not_serving", from: web2, to: web3, detail: "timeout", replacement_removed: false }), web2.id).reason)
      .toBe("Its new container on web-3 didn't become healthy, and may still be there. It still runs here.");
    expect(drainRow(failure({ stage: "old_not_removed", from: web2, to: web1, detail: "timeout", old_stopped: false }), web2.id).reason)
      .toBe("It now runs on web-1 too, but its container here couldn't be stopped.");
    expect(drainRow(failure({ stage: "old_not_removed", from: web2, to: web1, detail: "timeout", old_stopped: true }), web2.id).reason)
      .toBe("It now runs on web-1. Its container here stopped but couldn't be removed.");
    const refused: ServiceDrain = {
      service: "shop/worker", result: "failed", moves: [{ from: web2, to: web1 }], failure: { stage: "refused", reason: { kind: "mid_rollout" } },
    };
    expect(drainRow(refused, web2.id).reason).toBe("Stopped moving it. A deploy is in progress. Deploy it first. Moved to web-1 before that.");
  });

  it("says whether a cancelled Drain stopped before or during a move, and what a mid-move cancel may have left", () => {
    const failure = (failure: MoveFailure): ServiceDrain => ({ service: "shop/worker", result: "failed", moves: [], failure });
    expect(drainRow(failure({ stage: "cancelled_before_move", from: web2 }), web2.id).reason).toBe("The drain stopped before moving it.");
    expect(drainRow(failure({ stage: "cancelled", from: web2, to: web1, replacement_removed: true }), web2.id).reason)
      .toBe("The drain stopped mid-move. It still runs here.");
    expect(drainRow(failure({ stage: "cancelled", from: web2, to: web1, replacement_removed: false }), web2.id).reason)
      .toBe("The drain stopped mid-move. It still runs here. Its new container on web-1 may still be there.");
  });

  it("renders an outcome a newer Engine reports without crashing", () => {
    const future = JSON.parse('{"service":"shop/api","result":"teleported"}');
    expect(drainRow(future, web2.id)).toMatchObject({ name: "api", tone: "neutral", label: "Unknown outcome" });
    expect(drainRow({ service: "shop/web", result: "not_attempted" }, web2.id)).toMatchObject({ label: "Not attempted" });
  });
});

describe("Drain view", () => {
  it("is idle until a Drain is asked for, and reads starting until this tab's request arrives", () => {
    expect(view({})).toEqual({ kind: "idle" });
    expect(view({}, "a2")).toEqual({ kind: "starting" });
    expect(view({ [web2.id]: finished([moved]) }, "a2")).toEqual({ kind: "starting" });
    expect(view({ [web2.id]: { attemptId: "a2", state: "pending", requestedAt: "2026-10-04T10:00:00.000Z" } }, "a2"))
      .toEqual({ kind: "queued", behind: null });
  });

  it("names the Server whose running Drain a queued one waits behind", () => {
    expect(view({
      [web2.id]: { attemptId: "a2", state: "pending", requestedAt: "2026-10-04T10:00:00.000Z" },
      [web1.id]: { attemptId: "a1", state: "running", startedAt: "2026-10-04T09:59:00.000Z" },
    })).toEqual({ kind: "queued", behind: "web-1" });
  });

  it("offers Drain again when something stayed, failed or was not reached, or the Drain stopped early", () => {
    expect(drainButtonLabel(view({ [web2.id]: finished([moved, retired]) }))).toBe("Drain");
    expect(drainButtonLabel(view({ [web2.id]: finished([moved, volume]) }))).toBe("Drain again");
    const stopped = view({ [web2.id]: finished([moved], { kind: "entry_unreachable", detail: "timeout" }) });
    expect(stopped).toMatchObject({ kind: "finished", stoppedEarly: "Lost contact with your servers", again: true });
    expect(drainButtonLabel(view({
      [web2.id]: { attemptId: "a1", state: "unknown", endedAt: "2026-10-04T10:00:00.000Z", failureCode: "lost", failureMessage: "lost" },
    }))).toBe("Drain again");
  });

  it("shows the Engine's words only when it refused", () => {
    const failed = (failureCode: "refused" | "dispatch_failed") => view({
      [web2.id]: { attemptId: "a1", state: "failed", endedAt: "2026-10-04T10:00:00.000Z", failureCode, failureMessage: "no server named web-2" },
    });
    expect(failed("refused")).toMatchObject({ kind: "failed", words: "Drain didn't start", details: "no server named web-2" });
    expect(failed("dispatch_failed")).toMatchObject({ kind: "failed", words: "Drain didn't start. Try again.", details: null });
  });
});

describe("Drain hints", () => {
  const postgres = { identity: "shop/postgres", name: "postgres", namespace: "shop" };
  const running = [postgres, { identity: "shop/api", name: "api", namespace: "shop" }];
  const stray = { identity: "left-behind/old", name: "old", namespace: "left-behind" };
  const strays = new Set(["left-behind"]);
  const none = new Set<string>();

  it("suggests draining while Services remain, and says nothing while a Drain runs", () => {
    expect(removeHint(view({}), running, none)).toEqual({ kind: "drain", count: 2, unowned: [] });
    expect(removeHint(view({}), [], none)).toEqual({ kind: "none", unowned: [] });
    expect(removeHint(view({}, "a2"), running, none)).toEqual({ kind: "none", unowned: [] });
  });

  it("says why when only Services whose volume is here remain after a Drain", () => {
    expect(removeHint(view({ [web2.id]: finished([moved, volume]) }), [postgres], none))
      .toEqual({ kind: "pinned", names: ["postgres"], unowned: [] });
    expect(removeHint(view({ [web2.id]: finished([moved, volume]) }), running, none)).toEqual({ kind: "drain", count: 2, unowned: [] });
  });

  it("counts only what a Drain would move, and names apart what no Project owns", () => {
    expect(removeHint(view({}), [...running, stray], strays)).toEqual({ kind: "drain", count: 2, unowned: ["old"] });
    expect(removeHint(view({}), [stray], strays)).toEqual({ kind: "none", unowned: ["old"] });
    expect(removeHint(view({ [web2.id]: finished([moved, volume]) }), [postgres, stray], strays))
      .toEqual({ kind: "pinned", names: ["postgres"], unowned: ["old"] });
  });

  it("lists what runs here by name for the dialog, apart from what no Project owns", () => {
    expect(drainDialogNames([
      { identity: "shop/api", name: "api", namespace: "shop" },
      { identity: "shop-staging/api", name: "api", namespace: "shop-staging" },
      stray,
    ], strays)).toEqual({ drainable: ["api"], unowned: ["old"] });
  });
});
