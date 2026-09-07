import { describe, expect, it } from "vitest";
import type { RuntimeLensStatus } from "#/modules/runtime/runtime.collection";
import { getServerListState } from "./server-list-state";

describe("getServerListState", () => {
  const emptyStates = [
    ["connecting", "loading"],
    ["no_connection", "empty"],
    ["unavailable", "empty"],
    ["unreachable", "empty"],
    ["live_empty", "empty"],
    ["live_rows", "empty"],
  ] satisfies Array<[RuntimeLensStatus, string]>;

  it.each(emptyStates)(
    "maps %s to %s when no rows are visible",
    (runtimeStatus, kind) => {
      expect(
        getServerListState({
          rowCount: 0,
          visibleRowCount: 0,
          query: "",
          runtimeStatus,
          runtimeError: null,
        }).kind,
      ).toBe(kind);
    },
  );

  it("distinguishes no Cloud connection from a connected empty cluster", () => {
    const state = (runtimeStatus: RuntimeLensStatus) =>
      getServerListState({
        rowCount: 0,
        visibleRowCount: 0,
        query: "",
        runtimeStatus,
        runtimeError: null,
      });

    expect(state("no_connection")).toEqual({
      kind: "empty",
      title: "No Cloud Connection",
      description: "Add a server to create a cluster and connect it to Cloud",
      variant: "first-run",
    });
    expect(state("live_empty")).toMatchObject({ title: "No active servers" });
    expect(state("unreachable")).toMatchObject({
      title: "Can't reach the cluster",
    });
    expect(state("unreachable")).not.toMatchObject({
      title: "No active servers",
    });
  });

  it("shows stale runtime context when unavailable runtime rows stay visible", () => {
    expect(
      getServerListState({
        rowCount: 1,
        visibleRowCount: 1,
        query: "",
        runtimeStatus: "unavailable",
        runtimeError: "Runtime connection lost.",
      }),
    ).toEqual({
      kind: "rows",
      notice: {
        title: "Showing the last observed runtime state",
        description: "Runtime connection lost.",
      },
    });
  });

  it("keeps the Cloud connection notice when rows are visible", () => {
    expect(
      getServerListState({
        rowCount: 1,
        visibleRowCount: 1,
        query: "",
        runtimeStatus: "no_connection",
        runtimeError: null,
      }),
    ).toEqual({
      kind: "rows",
      notice: {
        title: "No Cloud Connection",
        description: "Add a server to create a cluster and connect it to Cloud",
      },
    });
  });

  it("uses last-observed wording when unavailable servers stay visible", () => {
    expect(
      getServerListState({
        rowCount: 2,
        visibleRowCount: 1,
        query: "web",
        runtimeStatus: "unavailable",
        runtimeError: "Credentials expired",
      }),
    ).toEqual({
      kind: "rows",
      notice: {
        title: "Showing the last observed runtime state",
        description: "Credentials expired",
      },
    });
  });

  it("keeps live rows free of connection notices", () => {
    expect(
      getServerListState({
        rowCount: 1,
        visibleRowCount: 1,
        query: "",
        runtimeStatus: "live_rows",
        runtimeError: null,
      }),
    ).toEqual({ kind: "rows" });
  });

  it("shows a search miss when loaded rows do not match", () => {
    expect(
      getServerListState({
        rowCount: 1,
        visibleRowCount: 0,
        query: "missing",
        runtimeStatus: "live_rows",
        runtimeError: null,
      }),
    ).toMatchObject({ kind: "empty", title: "No matches" });
  });

  it("keeps connection notices when rows are filtered out", () => {
    expect(
      getServerListState({
        rowCount: 1,
        visibleRowCount: 0,
        query: "missing",
        runtimeStatus: "no_connection",
        runtimeError: null,
      }),
    ).toMatchObject({
      kind: "empty",
      title: "No matches",
      notice: {
        title: "No Cloud Connection",
        description: "Add a server to create a cluster and connect it to Cloud",
      },
    });

    expect(
      getServerListState({
        rowCount: 1,
        visibleRowCount: 0,
        query: "missing",
        runtimeStatus: "unavailable",
        runtimeError: "Runtime connection lost.",
      }),
    ).toMatchObject({
      kind: "empty",
      title: "No matches",
      notice: {
        title: "Runtime unavailable",
        description: "Runtime connection lost.",
      },
    });
  });

  it("does not let search hide connection state", () => {
    const state = (runtimeStatus: RuntimeLensStatus) =>
      getServerListState({
        rowCount: 0,
        visibleRowCount: 0,
        query: "missing",
        runtimeStatus,
        runtimeError: null,
      });

    expect(state("connecting")).toEqual({ kind: "loading" });
    expect(state("no_connection")).toMatchObject({
      title: "No Cloud Connection",
    });
  });

  it("shows the generic runtime state for an establishment failure", () => {
    expect(
      getServerListState({
        rowCount: 0,
        visibleRowCount: 0,
        query: "",
        runtimeStatus: "unavailable",
        runtimeError: "Credentials expired",
      }),
    ).toMatchObject({ title: "Runtime unavailable" });
  });
});
