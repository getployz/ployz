import type { DeploymentView, NodeOutcome, RowPhase, ServerRow } from "@ployz/sdk";
import { describe, expect, it } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { machineId } from "#/test/ids";
import { makeRunGraph, rowLabel, rowPhaseLabel, selectRunNode } from "./run-graph";

const deployment = (fields: Partial<DeploymentView>) => asTestDouble<DeploymentView>()({
  id: "d1", status: "running", in_flight: true, builds: [], nodes: [], runtime_names: {}, preview: null, ...fields,
});
const at = { machine_id: machineId("1"), server: "fsn-1", started_at: 10, finished_at: null };
const running = (phase: RowPhase): ServerRow => ({ ...at, state: "running", phase, serving: false });
const completed = (server = "fsn-1", started_at = 10, finished_at = 20): ServerRow =>
  ({ ...at, server, state: "completed", started_at, finished_at });
const service = (name: string, rows: ServerRow[], outcome: NodeOutcome["outcome"] = "pending"): NodeOutcome =>
  ({ type: "service", id: `${name}-id`, name, outcome, rows });

describe("run graph", () => {
  it("mirrors the Deployment's builds and nodes, Volumes and Configs included", () => {
    const view = deployment({
      builds: [{ service: "api", commit: "abc", status: "building", message: null }],
      nodes: [
        service("api", [running("starting")]),
        { type: "volume", id: "v", name: "data", outcome: "deployed", rows: [completed()] },
        { type: "config", id: "c", name: "app", outcome: "unchanged", rows: [] },
        service("db", [], "failed"),
      ],
      runtime_names: { api: "private-api" },
      preview: asTestDouble<NonNullable<DeploymentView["preview"]>>()({ depths: { "private-api": 2, db: 0 } }),
    });
    const graph = makeRunGraph(view);
    expect(graph.nodes.map(({ type, id, name, light, column }) => ({ type, id, name, light, column }))).toEqual([
      { type: "build", id: "build:api", name: "api", light: "deploying", column: 0 },
      { type: "service", id: "api-id", name: "api", light: "deploying", column: 3 },
      { type: "volume", id: "v", name: "data", light: "deployed", column: 0 },
      { type: "config", id: "c", name: "app", light: "deployed", column: 0 },
      { type: "service", id: "db-id", name: "db", light: "failed", column: 1 },
    ]);
    expect(graph.done).toBe(2);
  });

  it("stops showing a build as running once the Deployment ended", () => {
    const view = deployment({ status: "failed", in_flight: false, builds: [{ service: "api", commit: null, status: "building", message: null }] });
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
      builds: [{ service: "web", commit: "abc", status: "built", message: null }],
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
    const waiting = { service: "api", commit: null, status: "pending" as const, message: null };
    const view = deployment({ builds: [waiting], nodes: [service("api", []), service("web", [running("starting")])] });
    expect(selectRunNode(makeRunGraph(view), {})?.id).toBe("web-id");
    const building = deployment({ builds: [waiting, { ...waiting, service: "web", status: "building" }], nodes: [service("api", [])] });
    expect(selectRunNode(makeRunGraph(building), {})?.id).toBe("build:web");
    expect(selectRunNode(makeRunGraph({ ...view, nodes: view.nodes.slice(0, 1) }), {})?.id).toBe("build:api");
  });
});
