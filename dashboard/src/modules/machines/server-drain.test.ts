import type { DrainReport, MachineRef, Move, MoveFailure, ServiceDrain } from "@ployz/sdk";
import { describe, expect, it } from "vitest";
import type { LatestDrain, LatestDrains } from "#/modules/machines/server-drain";
import {
  drainButtonLabel,
  drainDialogRows,
  drainLeftRows,
  drainRow,
  drainSummary,
  drainView,
  removeHint,
  stuckMessage,
} from "#/modules/machines/server-drain-view";
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
  remaining: DrainReport["remaining"] = { kind: "observed", services: [], unchosen: [] },
): DrainReport => ({
  server: runtimeWatchMachineFixture(web2.id, web2.name),
  services_role: "turned_off",
  services,
  stopped,
  remaining,
  // As the Engine derives it.
  complete: stopped === null && remaining.kind === "observed"
    && services.every(serviceComplete),
});
// As `DrainOutcome::complete` derives it: a move or a leftover counts only once its old container is stopped.
const serviceComplete = (entry: ServiceDrain) => {
  switch (entry.result) {
    case "moved":
      return entry.moves.every((move) => move.old.kind === "stopped");
    case "old_copies":
      return entry.ends.every((end) => end.kind === "stopped");
    case "nothing_to_move":
    case "retired":
      return true;
    default:
      return false;
  }
};
const finished = (services: ServiceDrain[], stopped: DrainReport["stopped"] = null): LatestDrain => ({
  attemptId: "a1", state: "finished", endedAt: "2026-10-04T10:00:00.000Z", report: report(services, stopped),
});
const view = (latest: LatestDrains, requested: string | null = null) => drainView({ server, latest, serverNames, requested });

const moved: ServiceDrain = { service: "shop/api", result: "moved", moves: [{ from: web2, to: web1, old: { kind: "stopped" } }, { from: web2, to: web3, old: { kind: "stopped" } }, { from: web2, to: web1, old: { kind: "stopped" } }] };
const retired: ServiceDrain = { service: "monitoring/node-exporter", result: "retired" };
const volume: ServiceDrain = { service: "shop/postgres", result: "stays", reason: { kind: "volume", server: web2 } };
const rollout: ServiceDrain = { service: "shop/redis", result: "stays", reason: { kind: "mid_rollout" } };

describe("Drain result", () => {
  it("counts what moved, stopped, stayed and failed, in that order", () => {
    const failed: ServiceDrain = { service: "shop/worker", result: "failed", moves: [], failure: { stage: "no_destination", from: web2, detail: "no room" } };
    expect(drainSummary(report([moved, retired, volume, rollout, failed]), "web-2")).toBe("1 moved, 1 stopped, 2 stayed, 1 failed");
    expect(drainSummary(report([moved, retired]), "web-2")).toBe("Everything moved off web-2");
    expect(drainSummary(report([]), "web-2")).toBe("Nothing was running here");
    expect(drainSummary(report([{ service: "shop/api", result: "nothing_to_move" }]), "web-2")).toBe("Nothing needed to move");
    expect(drainSummary(report([{ service: "shop/cron", result: "old_copies", ends: [{ kind: "stopped" }] }]), "web-2")).toBe("Everything moved off web-2");
    expect(drainSummary(report([{ service: "shop/cron", result: "old_copies", ends: [{ kind: "running", unconfirmed: { kind: "proxies", servers: [web1] } }] }]), "web-2")).toBe("1 stayed");
  });

  it("says everything moved off when only another server's move failed, and still shows that failure", () => {
    const elsewhere: ServiceDrain = {
      service: "shop/api",
      result: "failed",
      moves: [{ from: web2, to: web1, old: { kind: "stopped" } }],
      failure: { stage: "read_image", from: web3, to: web1, detail: "web-3 did not answer" },
    };
    // The Engine counts it complete: the failure left nothing behind, and nothing remains here.
    const done = { ...report([elsewhere]), complete: true };
    expect(drainSummary(done, "web-2")).toBe("Everything moved off web-2");
    expect(drainRow(elsewhere, web2.id)).toMatchObject({ tone: "failed", reason: expect.stringContaining("It still runs on web-3.") });
  });

  it("counts what it left alone and never says nothing ran here while something does", () => {
    const unowned = ["left-behind/old", "legacy/cron"];
    const left: DrainReport["remaining"] = { kind: "observed", services: [...unowned, "ployz-system/dns"], unchosen: unowned };
    expect(drainSummary(report([], null, left), "web-2")).toBe("2 left alone");
    const stayed: DrainReport["remaining"] = { kind: "observed", services: ["left-behind/old", "shop/postgres"], unchosen: ["left-behind/old"] };
    expect(drainSummary(report([moved, volume], null, stayed), "web-2")).toBe("1 moved, 1 stayed, 1 left alone");
    const system: DrainReport["remaining"] = { kind: "observed", services: ["ployz-system/dns"], unchosen: [] };
    expect(drainSummary(report([], null, system), "web-2")).toBe("Nothing was running here");
  });

  it("says when it couldn't check what's left, and offers Drain again", () => {
    const unobserved = { kind: "unobserved", error: "timeout" } as const;
    expect(drainSummary(report([], null, unobserved), "web-2")).toBe("Couldn't check what's left on web-2");
    expect(drainSummary(report([moved], null, unobserved), "web-2")).toBe("1 moved, but couldn't check what's left");
    const own: LatestDrain = { attemptId: "a1", state: "finished", endedAt: "2026-10-04T10:00:00.000Z", report: report([moved], null, unobserved) };
    expect(drainButtonLabel(view({ [web2.id]: own }))).toBe("Drain again");
  });

  it("names each destination once, says why a Service stayed, and stops a Global here", () => {
    expect(drainRow(moved, web2.id)).toMatchObject({ name: "api", namespace: "shop", tone: "moved", label: "Moved to web-1 and web-3", reason: null });
    expect(drainRow(volume, web2.id)).toMatchObject({ tone: "stayed", label: "Stayed", reason: "Its volume is on this server", pinned: true });
    expect(drainRow(rollout, web2.id)).toMatchObject({ reason: "A deploy is in progress. Deploy it first.", pinned: false });
    expect(drainRow(retired, web2.id)).toMatchObject({ global: true, tone: "stopped", label: "Stopped here", reason: "Runs on every server" });
  });

  it("keeps the moves a failed Service made before it failed, and never claims an unremoved container still serves only here", () => {
    const partial: ServiceDrain = {
      service: "shop/worker",
      result: "failed",
      moves: [{ from: web2, to: web1, old: { kind: "stopped" } }],
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
    const refused: ServiceDrain = {
      service: "shop/worker", result: "failed", moves: [{ from: web2, to: web1, old: { kind: "stopped" } }], failure: { stage: "refused", reason: { kind: "mid_rollout" } },
    };
    expect(drainRow(refused, web2.id).reason).toBe("Stopped moving it. A deploy is in progress. Deploy it first. Moved to web-1 before that.");
  });

  it("says what an old container left on the drained server means, and what a user-stopped one needs", () => {
    const left = (old: Move["old"]): ServiceDrain => ({ service: "shop/web", result: "moved", moves: [{ from: web2, to: web1, old }] });
    expect(drainRow(left({ kind: "running", unconfirmed: { kind: "proxies", servers: [web3] } }), web2.id)).toMatchObject({
      tone: "moved",
      label: "Moved to web-1",
      reason: "Its old container here is still running: web-3 didn't confirm the switch. The next drain cleans it up.",
    });
    expect(drainRow(left({ kind: "in", unconfirmed: { kind: "cancelled" } }), web2.id).reason)
      .toBe("Its old container here still serves too: the drain stopped before the switch was confirmed. The next drain cleans it up.");
    expect(drainRow({ service: "shop/web", result: "old_copies", ends: [{ kind: "stopped" }] }, web2.id))
      .toMatchObject({ tone: "stopped", label: "Stopped here", reason: "Removed what an earlier move left here" });
    const held: ServiceDrain = { service: "shop/worker", result: "held" };
    expect(drainRow(held, web2.id)).toMatchObject({ tone: "stayed", label: "Stayed", reason: "You stopped it. Start or remove it first." });
    expect(drainRow({ service: "shop/web", result: "stays", reason: { kind: "last_in" } }, web2.id).reason)
      .toBe("It's the last copy serving. Deploy it to another server first.");
    expect(drainSummary(report([moved, held, { service: "shop/web", result: "old_copies", ends: [{ kind: "stopped" }] }]), "web-2"))
      .toBe("1 moved, 1 stopped, 1 stayed");
  });

  it("says where a stopped Drain left a Service, and what a mid-move cancel may have left", () => {
    expect(drainRow({ service: "shop/worker", result: "interrupted", moves: [] }, web2.id))
      .toMatchObject({ tone: "neutral", label: "Interrupted", reason: "The drain stopped before moving it. It still runs here." });
    expect(drainRow({ service: "shop/worker", result: "interrupted", moves: [{ from: web2, to: web1, old: { kind: "stopped" } }] }, web2.id).reason)
      .toBe("The drain stopped while moving it. Moved to web-1 before that.");
    expect(drainSummary(report([moved, { service: "shop/worker", result: "interrupted", moves: [] }], { kind: "cancelled" }), "web-2"))
      .toBe("1 moved, 1 interrupted");
    const failure = (failure: MoveFailure): ServiceDrain => ({ service: "shop/worker", result: "failed", moves: [], failure });
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
    expect(view({ [web2.id]: { attemptId: "a2", state: "pending", requestedAt: "2026-10-04T10:00:00.000Z", serversSilent: false } }, "a2"))
      .toEqual({ kind: "queued", behind: null });
  });

  it("names the Server whose running Drain a queued one waits behind", () => {
    expect(view({
      [web2.id]: { attemptId: "a2", state: "pending", requestedAt: "2026-10-04T10:00:00.000Z", serversSilent: false },
      [web1.id]: { attemptId: "a1", state: "running", startedAt: "2026-10-04T09:59:00.000Z" },
    })).toEqual({ kind: "queued", behind: "web-1" });
  });

  it("reads waiting for its servers once they didn't answer it, whether or not another Drain runs", () => {
    const waiting: LatestDrain = { attemptId: "a2", state: "pending", requestedAt: "2026-10-04T10:00:00.000Z", serversSilent: true };
    expect(view({ [web2.id]: waiting })).toEqual({ kind: "waiting" });
    expect(view({
      [web2.id]: waiting,
      [web1.id]: { attemptId: "a1", state: "running", startedAt: "2026-10-04T09:59:00.000Z" },
    })).toEqual({ kind: "waiting" });
  });

  it("offers Drain again when something stayed, failed or was not reached, or the Drain stopped early", () => {
    expect(drainButtonLabel(view({ [web2.id]: finished([moved, retired]) }))).toBe("Drain");
    expect(drainButtonLabel(view({ [web2.id]: finished([moved, volume]) }))).toBe("Drain again");
    const stopped = view({ [web2.id]: finished([moved], { kind: "entry_unreachable", detail: "timeout" }) });
    expect(stopped).toMatchObject({ kind: "finished", stoppedEarly: "Lost contact with your servers", again: true });
    expect(drainButtonLabel(view({
      [web2.id]: { attemptId: "a1", state: "unknown", endedAt: "2026-10-04T10:00:00.000Z", endCode: "lost" },
    }))).toBe("Drain again");
  });

  it("shows the Engine's words only when it refused, and says what each other end means", () => {
    const at = "2026-10-04T10:00:00.000Z";
    const ended = (drain: LatestDrain) => view({ [web2.id]: drain });
    expect(ended({ attemptId: "a1", state: "failed", endedAt: at, endCode: "refused", refusalMessage: "no server named web-2" }))
      .toEqual({ kind: "failed", at, words: "Drain didn't start", details: "no server named web-2" });
    expect(ended({ attemptId: "a1", state: "failed", endedAt: at, endCode: "not_started" }))
      .toEqual({ kind: "failed", at, words: "Drain didn't start. Try again.", details: null });
    expect(ended({ attemptId: "a1", state: "cancelled", endedAt: at, endCode: "cancelled" })).toEqual({ kind: "cancelled", at });
    expect(ended({ attemptId: "a1", state: "unknown", endedAt: at, endCode: "interrupted" }))
      .toEqual({ kind: "unknown", at, words: "Drain was cancelled. Running here shows what's still on this server." });
  });
});

describe("Drain ends", () => {
  it("lets an end carry only its own state, and words only for a refusal", () => {
    const at = "2026-10-04T10:00:00.000Z";
    const ends: LatestDrain[] = [
      // @ts-expect-error a lost Drain may have moved anything: it is unknown, never failed
      { attemptId: "a1", state: "failed", endedAt: at, endCode: "lost" },
      // @ts-expect-error only the Engine's refusal has words
      { attemptId: "a1", state: "failed", endedAt: at, endCode: "not_started", refusalMessage: "no" },
      // @ts-expect-error a refusal always has the Engine's words
      { attemptId: "a1", state: "failed", endedAt: at, endCode: "refused" },
    ];
    expect(ends).toHaveLength(3);
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

  it("lists what runs here for the dialog, warning on what the latest Drain left for its volume and what no Project owns", () => {
    const api = { identity: "shop/api", name: "api", namespace: "shop" };
    const rows = drainDialogRows(view({ [web2.id]: finished([moved, volume]) }), web2.id, [postgres, stray, api], strays);
    expect(rows).toEqual([
      { key: "shop/api", name: "api", namespace: "shop", stays: null },
      { key: "shop/postgres", name: "postgres", namespace: "shop", stays: "data" },
      { key: "left-behind/old", name: "old", namespace: "left-behind", stays: "unowned" },
    ]);
    expect(drainDialogRows(view({}), web2.id, [postgres], none).map((row) => row.stays)).toEqual([null]);
  });

  it("marks a Global Service as stopping when no other Server runs a copy, even when none runs here", () => {
    const global = (name: string, runningOn: string[]) =>
      ({ identity: `shop/${name}`, name, namespace: "shop", global: true, runningOn: new Set(runningOn) });
    const rows = drainDialogRows(view({}), web2.id,
      [global("agent", [web2.id]), global("crashed", []), global("spread", [web2.id, "elsewhere"])], none);
    expect(rows.map((row) => [row.name, row.stays])).toEqual([["spread", null], ["agent", "stops"], ["crashed", "stops"]]);
  });

  it("lists what a Drain under way still has to move, without what no Project owns", () => {
    expect(drainLeftRows([postgres, stray], strays).map((row) => [row.key, row.label])).toEqual([["shop/postgres", "Pending"]]);
  });
});

describe("A leftover still running here", () => {
  const leftover: ServiceDrain = { service: "shop/web", result: "old_copies", ends: [{ kind: "running", unconfirmed: { kind: "cancelled" } }] };

  it("reads as still running, not stopped", () => {
    expect(drainRow(leftover, web2.id)).toMatchObject({ tone: "stayed", label: "Still running here" });
  });

  it("counts as stayed, not stopped", () => {
    const value = report([leftover], null, { kind: "observed", services: ["shop/web"], unchosen: [] });
    expect(drainSummary(value, "web-2")).toBe("1 stayed");
  });
});

describe("stuckMessage", () => {
  it("names the Server whose upgrade would move each Volume there, and says the rest have nowhere to move", () => {
    expect(stuckMessage([{ volume: "db", upgrade: null }])).toBe("db has nowhere to move");
    expect(stuckMessage([{ volume: "db", upgrade: "web-2" }])).toBe("Upgrade web-2 to move db there");
    expect(stuckMessage([{ volume: "db", upgrade: "web-2" }, { volume: "cache", upgrade: null }, { volume: "logs", upgrade: "web-2" }]))
      .toBe("cache has nowhere to move. Upgrade web-2 to move db and logs there");
    expect(stuckMessage([{ volume: "db", upgrade: "web-2" }, { volume: "cache", upgrade: "web-3" }]))
      .toBe("Upgrade web-2 to move db there. Upgrade web-3 to move cache there");
  });

  it("keeps a Volume nowhere takes when another Environment's Volume of the same name can move once a Server is upgraded", () => {
    expect(stuckMessage([{ volume: "data", upgrade: "web-2" }, { volume: "data", upgrade: null }]))
      .toBe("data has nowhere to move. Upgrade web-2 to move data there");
  });
});
