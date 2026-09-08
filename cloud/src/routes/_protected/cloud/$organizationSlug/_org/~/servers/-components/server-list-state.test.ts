import { describe, expect, it } from "vitest";
import type { RuntimeLensStatus } from "#/modules/runtime/runtime.collection";
import {
  getServerListState,
  incompleteRuntimeObservationDescription,
} from "./server-list-state";

describe("getServerListState", () => {
  const emptyStates = [
    ["connecting", "loading"],
    ["no_connection", "empty"],
    ["unavailable", "empty"],
    ["unreachable", "empty"],
    ["observed", "empty"],
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

  it("distinguishes no Cloud connection from an empty Runtime Watch observation", () => {
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
    expect(state("observed")).toMatchObject({
      title: "No servers in the latest observation",
    });
    expect(state("unreachable")).toMatchObject({
      title: "Can't reach the cluster",
    });
    expect(state("unreachable")).not.toMatchObject({
      title: "No servers in the latest observation",
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
        runtimeStatus: "observed",
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
        runtimeStatus: "observed",
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

describe("incompleteRuntimeObservationDescription", () => {
  it("keeps incomplete IDs visible beside observed workload counts", () => {
    expect(
      incompleteRuntimeObservationDescription({
        machines: ["machine-1"],
        containers: ["container-1", "container-2"],
        volumes: [],
        certificates: ["api.example.com"],
      }),
    ).toBe(
      "The Runtime Watch lists incomplete IDs for 1 machine, 2 containers, 1 certificate. Server workload counts include only containers it observed.",
    );
  });

  it("does not show an uncertainty notice for a complete observation", () => {
    expect(
      incompleteRuntimeObservationDescription({
        machines: [],
        containers: [],
        volumes: [],
        certificates: [],
      }),
    ).toBeNull();
  });
});
