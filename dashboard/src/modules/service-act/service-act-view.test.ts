import { describe, expect, it } from "vitest";
import type { ServiceActProgress } from "./service-act";
import { actAlert, actBadge } from "./service-act-view";

const restart: ServiceActProgress = { id: "act-1", verb: "restart", state: "running", total: 12, done: 4, reason: null, scoped: false };

describe("actBadge", () => {
  it("counts a running restart's containers", () => {
    expect(actBadge(restart, null)).toBe("Restarting 4/12");
  });

  it("reads a click as started before its row arrives, over the last run", () => {
    expect(actBadge({ ...restart, state: "done" }, "stop")).toBe("Stopping…");
  });

  it("doesn't count a stop or a start", () => {
    expect(actBadge({ ...restart, verb: "start" }, null)).toBe("Starting…");
  });

  it("says nothing once the run ended", () => {
    expect(actBadge({ ...restart, state: "failed" }, null)).toBeNull();
    expect(actBadge(null, null)).toBeNull();
  });
});

describe("actAlert", () => {
  it("says why a restart stopped, with Retry running it again", () => {
    const reason = "Container 4 on web-2 didn't come back. The rest are untouched.";
    expect(actAlert({ ...restart, state: "failed", reason })).toEqual({ title: "Restart stopped", description: reason, retry: "restart" });
  });

  it("offers no Retry for a run that targeted one server, since a retry here runs on every server", () => {
    expect(actAlert({ ...restart, state: "failed", scoped: true })).toMatchObject({ title: "Restart stopped", retry: null });
  });

  it("offers no Retry for a failed stop, so a stop always goes through its confirm", () => {
    expect(actAlert({ ...restart, verb: "stop", state: "failed" })).toMatchObject({ title: "Stop failed", retry: null });
  });

  it("shows nothing while a run moves or once it ended done", () => {
    expect(actAlert(restart)).toBeNull();
    expect(actAlert({ ...restart, state: "done" })).toBeNull();
  });
});
