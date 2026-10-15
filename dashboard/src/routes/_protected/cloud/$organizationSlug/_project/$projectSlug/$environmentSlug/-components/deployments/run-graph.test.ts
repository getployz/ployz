import type { BuildView, DeploymentView, NodeOutcome, RowPhase, ServerRow } from "@ployz/sdk";
import { describe, expect, it } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { machineId } from "#/test/ids";
import { makeRunGraph, orderRows, rowLabel, rowPhaseLabel, selectRunNode } from "./run-graph";

const deployment = (fields: Partial<DeploymentView>) => asTestDouble<DeploymentView>()({
  id: "d1", status: "running", in_flight: true, builds: [], nodes: [], runtime_names: {}, preview: null, ...fields,
});
const preview = (fields: Partial<NonNullable<DeploymentView["preview"]>>) =>
  asTestDouble<NonNullable<DeploymentView["preview"]>>()({ depths: {}, dependencies: [], ...fields });
const build = (service: string, fields: Partial<BuildView> = {}): BuildView =>
  ({ service, commit: null, status: "built", message: null, started_at: null, finished_at: null, ...fields });
const at = { machine_id: machineId("1"), server: "fsn-1", started_at: 10, finished_at: null, history: null };
const running = (phase: RowPhase): ServerRow => ({ ...at, state: "running", phase, serving: false });
const completed = (server = "fsn-1", started_at = 10, finished_at = 20): ServerRow =>
  ({ ...at, server, state: "completed", started_at, finished_at });
const service = (name: string, rows: ServerRow[], outcome: NodeOutcome["outcome"] = "pending"): NodeOutcome =>
  ({ type: "service", id: `${name}-id`, name, outcome, rows });

describe("run graph", () => {
  it("mirrors the Deployment's builds and nodes, Volumes and Configs included", () => {
    const view = deployment({
      builds: [build("api", { status: "building" })],
      nodes: [
        service("api", [running("starting")]),
        { type: "volume", id: "v", name: "data", outcome: "deployed", rows: [completed()] },
        { type: "config", id: "c", name: "app", outcome: "unchanged", rows: [] },
        service("db", [], "failed"),
      ],
      runtime_names: { api: "private-api" },
      preview: preview({ depths: { "private-api": 2, db: 0 } }),
    });
    const graph = makeRunGraph(view);
    expect(graph.nodes.map(({ type, id, name, light, column }) => ({ type, id, name, light, column }))).toEqual([
      { type: "build", id: "build:api", name: "api", light: "deploying", column: 2 },
      { type: "service", id: "api-id", name: "api", light: "deploying", column: 3 },
      { type: "volume", id: "v", name: "data", light: "deployed", column: 0 },
      { type: "config", id: "c", name: "app", light: "deployed", column: 0 },
      { type: "service", id: "db-id", name: "db", light: "failed", column: 1 },
    ]);
    expect(graph.done).toBe(2);
  });

  it("stops showing a build as running once the Deployment ended", () => {
    const view = deployment({ status: "failed", in_flight: false, builds: [build("api", { status: "building" })] });
    expect(makeRunGraph(view).nodes[0]?.light).toBe("not_applied");
  });

  it("times a node over its rows only when every row says when it started", () => {
    const view = deployment({ nodes: [service("api", [completed("fsn-1", 10, 20), completed("fsn-2", 12, 42)], "deployed")] });
    expect(makeRunGraph(view).nodes[0]).toMatchObject({ timing: { started: 10, finished: 42 } });
    const unstarted = { ...completed("fsn-2"), started_at: null };
    expect(makeRunGraph({ ...view, nodes: [service("api", [completed(), unstarted], "deployed")] }).nodes[0]).toMatchObject({ timing: undefined });
  });

  it("words each row phase and a failed row's step", () => {
    expect(rowPhaseLabel("waiting_for_switch")).toBe("Waiting for switch");
    expect(rowPhaseLabel("compensating")).toBe("Rolling back");
    expect(rowLabel(running("waiting_for_health"))).toBe("Waiting for health");
    expect(rowLabel({ ...at, state: "failed", reason: "r", cause: [], step: "health", log: null })).toBe("Failed at health check");
    expect(rowLabel({ ...at, state: "failed", reason: "r", cause: [], step: null, log: null })).toBe("Failed");
    expect(rowLabel(completed())).toBe("Done");
  });

  it("opens the picked node, then the origin's, then the first failed, running, or Service", () => {
    const view = deployment({
      builds: [build("web")],
      nodes: [service("api", [completed()], "deployed"), service("web", [running("starting")]), service("db", [], "failed")],
    });
    const graph = makeRunGraph(view);
    const origin = { projectSlug: "shop", environmentSlug: "production", node: { kind: "service" as const, id: "api-id" } };
    expect(selectRunNode(graph, { job: "build:web", origin })?.id).toBe("build:web");
    expect(selectRunNode(graph, { job: "gone", origin })?.id).toBe("api-id");
    expect(selectRunNode(graph, { origin: { ...origin, node: { kind: "volume", id: "api-id" } } })?.id).toBe("db-id");
    expect(selectRunNode(graph, {})?.id).toBe("db-id");
    expect(selectRunNode(makeRunGraph({ ...view, nodes: view.nodes.slice(0, 2) }), {})?.id).toBe("web-id");
    expect(selectRunNode(makeRunGraph({ ...view, nodes: view.nodes.slice(0, 1) }), {})?.id).toBe("api-id");
    expect(selectRunNode(makeRunGraph(deployment({})), {})).toBeUndefined();
  });

  it("opens the node doing work over one still waiting its turn", () => {
    const waiting = build("api", { status: "pending" });
    const view = deployment({ builds: [waiting], nodes: [service("api", []), service("web", [running("starting")])] });
    expect(selectRunNode(makeRunGraph(view), {})?.id).toBe("web-id");
    const building = deployment({ builds: [waiting, { ...waiting, service: "web", status: "building" }], nodes: [service("api", [])] });
    expect(selectRunNode(makeRunGraph(building), {})?.id).toBe("build:web");
    expect(selectRunNode(makeRunGraph({ ...view, nodes: view.nodes.slice(0, 1) }), {})?.id).toBe("build:api");
  });

  const chain = deployment({
    builds: [build("web"), build("api")],
    nodes: [
      service("web", [running("starting")]),
      { type: "volume", id: "v", name: "data", outcome: "deployed", rows: [completed()] },
      service("api", [completed()], "deployed"),
      service("postgres", [completed()], "deployed"),
    ],
    runtime_names: { api: "private-api" },
    preview: preview({
      depths: { postgres: 0, "private-api": 1, web: 2 },
      dependencies: [["postgres", "private-api"], ["private-api", "web"], ["postgres", "private-api"], ["postgres", "gone"]],
    }),
  });

  it("links each build to its Service and each dependency to its dependent, animated while the dependent runs", () => {
    expect(makeRunGraph(chain).edges).toEqual([
      { id: "build:web->web-id", source: "build:web", target: "web-id", animated: true },
      { id: "build:api->api-id", source: "build:api", target: "api-id", animated: false },
      { id: "postgres-id->api-id", source: "postgres-id", target: "api-id", animated: false },
      { id: "api-id->web-id", source: "api-id", target: "web-id", animated: true },
    ]);
    expect(makeRunGraph({ ...chain, builds: [], preview: null }).edges).toEqual([]);
    const waiting = { ...chain, nodes: [service("web", []), ...chain.nodes.slice(1)] };
    expect(makeRunGraph(waiting).edges.filter((edge) => edge.animated)).toEqual([]);
  });

  it("keeps Volumes and Configs in the first column, unlinked, since nothing says which Service mounts them", () => {
    const graph = makeRunGraph(chain);
    expect(graph.nodes.find((node) => node.id === "v")).toMatchObject({ column: 0, row: 0 });
    expect(graph.edges.some((edge) => edge.source === "v" || edge.target === "v")).toBe(false);
  });

  it("puts each build just before its Service and orders each column by the rows linking into it", () => {
    const rows = makeRunGraph(chain).nodes.map(({ id, column, row }) => ({ id, column, row }));
    expect(rows).toEqual([
      { id: "build:web", column: 2, row: 0.5 },
      { id: "build:api", column: 1, row: -0.5 },
      { id: "web-id", column: 3, row: 0 },
      { id: "v", column: 0, row: 0 },
      { id: "api-id", column: 2, row: -0.5 },
      { id: "postgres-id", column: 1, row: 0.5 },
    ]);
    const node = (id: string, column: number, rank = 0) => ({ id, name: id, column, rank });
    const ordered = orderRows(
      [node("b", 0), node("a", 0), node("c", 0, 1), node("z", 1), node("x", 1), node("w", 1), node("y", 1), node("v", 1)],
      [{ source: "c", target: "x" }, { source: "a", target: "y" }, { source: "c", target: "y" }, { source: "b", target: "v" }],
    );
    expect(Object.fromEntries(ordered)).toEqual({ a: -1, b: 0, c: 1, v: -2, y: -1, x: 0, w: 1, z: 2 });
  });

  it("times a build from its start and finish, and never a row that was not attempted", () => {
    const timed = makeRunGraph(deployment({
      builds: [build("api", { started_at: 5, finished_at: 17 }), build("web", { status: "building", started_at: 8 }), build("db")],
    }));
    expect(timed.nodes.map((node) => node.timing)).toEqual([{ started: 5, finished: 17 }, { started: 8, finished: null }, undefined]);
    const skipped: ServerRow = {
      ...at, state: "not_attempted", started_at: null, finished_at: 30, history: [{ at: 3, state: "pending" }, { at: 30, state: "not_attempted" }],
    };
    expect(makeRunGraph(deployment({ nodes: [service("web", [skipped], "not_attempted")] })).nodes[0]?.timing).toBeUndefined();
  });

  it("stops a build's clock when its Deployment ended before the build finished", () => {
    const ended = deployment({ status: "unknown", in_flight: false, builds: [build("api", { status: "building", started_at: 8 })] });
    expect(makeRunGraph(ended).nodes[0]?.timing).toBeUndefined();
  });

  it("keeps a deployed Service running while its old copies still stop", () => {
    const stopping = service("api", [completed(), running("stopping_container")], "deployed");
    expect(makeRunGraph(deployment({ nodes: [stopping] })).nodes[0]?.light).toBe("deploying");
    expect(makeRunGraph(deployment({ status: "applied", in_flight: false, nodes: [stopping] })).nodes[0]?.light).toBe("deployed");
    expect(makeRunGraph(deployment({ nodes: [service("api", [completed()], "deployed")] })).nodes[0]?.light).toBe("deployed");
  });

  it("keeps a deployed Volume deployed while another Service mounting it still runs", () => {
    const volume: NodeOutcome = { type: "volume", id: "v", name: "data", outcome: "deployed", rows: [completed(), running("starting")] };
    const graph = makeRunGraph(deployment({ nodes: [volume] }));
    expect(graph.nodes[0]?.light).toBe("deployed");
    expect(graph.done).toBe(1);
  });
});
