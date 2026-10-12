import type { ConfigView, ConfigWritten, JsonValue } from "@ployz/sdk";
import { describe, expect, it } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import type { SupportShows } from "./support-access";
import type { StoreRefusal } from "#/modules/config-store/store.contract";
import { holds, refusalForSupport, viewForSupport, withoutCheckOutput, writtenForSupport } from "./support-view";

const NONE: SupportShows = { variables: false, configs: false, logs: false };
const ALL: SupportShows = { variables: true, configs: true, logs: true };
const API = "030cc269-0427-4362-83e3-4ddaaab6ce96";

const view = asTestDouble<ConfigView>();

describe("what a Store cell holds", () => {
  it("reads it from the row, and from the path when a change has no row", () => {
    expect(holds(`${API}:variables.LOG_LEVEL`)).toBe("variable");
    expect(holds("8aa5:files.config.yml")).toBe("file");
    expect(holds(`${API}:node`)).toBe("other");
    expect(holds(null, "api.env.LOG_LEVEL")).toBe("variable");
    expect(holds(null, "api.env.LOG_LEVEL.exported")).toBe("other");
    expect(holds(null, "api.variables.LOG_LEVEL")).toBe("variable");
    expect(holds(null, "configs.@8aa5.files.config.yml")).toBe("file");
    expect(holds(null, "api.replicas")).toBe("other");
  });
});

describe("a Service's view for support", () => {
  const service = view({
    view: "service",
    values: { env: { LOG_LEVEL: "debug", SECRET_KEY: { exported: true, value: { secret: true } }, SHARED: { exported: true, value: "on" } }, replicas: 1 },
    changes: [
      { after: "debug", before: "trace", canRestore: true, kind: "change", path: "api.env.LOG_LEVEL", row: `${API}:variables.LOG_LEVEL` },
      { after: 2, before: 1, canRestore: true, kind: "change", path: "api.replicas", row: null },
    ],
  });

  it("seals plain values, exported ones too, and leaves everything else", () => {
    expect(viewForSupport(service, NONE)).toEqual({
      view: "service",
      values: { env: { LOG_LEVEL: { secret: true }, SECRET_KEY: { exported: true, value: { secret: true } }, SHARED: { exported: true, value: { secret: true } } }, replicas: 1 },
      changes: [
        { after: { secret: true }, before: { secret: true }, canRestore: true, kind: "change", path: "api.env.LOG_LEVEL", row: `${API}:variables.LOG_LEVEL` },
        { after: 2, before: 1, canRestore: true, kind: "change", path: "api.replicas", row: null },
      ],
    });
  });

  it("shows it whole once variables are on", () => {
    expect(viewForSupport(service, ALL)).toEqual(service);
  });

  it("keeps whether a variable is exported, which shares the variable's row", () => {
    const exported = view({
      view: "service",
      values: {},
      changes: [{ after: true, before: false, canRestore: true, kind: "change", path: "api.env.SHARED.exported", row: `${API}:variables.SHARED` }],
    });
    expect(viewForSupport(exported, NONE)).toEqual(exported);
  });
});

describe("a Sync's rows for support", () => {
  it("seals both sides of a variable and leaves unset ones unset", () => {
    const sync = view({
      view: "sync",
      rows: [
        { change: "changed", from: "trace", into: "debug", name: "env.LOG_LEVEL", node: "api", row: `${API}:variables.LOG_LEVEL` },
        { change: "new", from: "fresh", into: null, name: "env.NEW", node: "api", row: `${API}:variables.NEW` },
        { change: "changed", from: "whoami:2", into: "whoami:1", name: "image", node: "api", row: `${API}:image` },
      ],
    });
    expect(viewForSupport(sync, NONE)).toEqual({
      view: "sync",
      rows: [
        { change: "changed", from: { secret: true }, into: { secret: true }, name: "env.LOG_LEVEL", node: "api", row: `${API}:variables.LOG_LEVEL` },
        { change: "new", from: { secret: true }, into: null, name: "env.NEW", node: "api", row: `${API}:variables.NEW` },
        { change: "changed", from: "whoami:2", into: "whoami:1", name: "image", node: "api", row: `${API}:image` },
      ],
    });
  });
});

describe("a Deployment for support", () => {
  const failed = { machine_id: "m1", server: "one", started_at: 1, finished_at: 2, state: "failed", reason: "exit 1", cause: [], step: null, log: "SECRET=hunter2" };
  const deployment = view({ view: "deployment", nodes: [{ type: "service", id: API, name: "api", rows: [failed] }] });

  it("drops a failed Server's container log while logs are off, and keeps why it failed", () => {
    expect(viewForSupport(deployment, NONE)).toEqual({
      view: "deployment", nodes: [{ type: "service", id: API, name: "api", rows: [{ ...failed, log: null }] }],
    });
    expect(viewForSupport(deployment, ALL)).toEqual(deployment);
  });
});

describe("a refusal for support", () => {
  it("seals the review it carries and leaves other details alone", () => {
    const setting = { after: "debug", before: null, canRestore: true, kind: "add", path: "api.env.LOG_LEVEL", row: `${API}:variables.LOG_LEVEL` };
    const diff = { view: "diff", changes: [{ name: "api", settings: [setting] }], follow_hints: [] };
    expect(refusalForSupport({ code: "approval_required", message: "Approve it.", details: { approval: "d1", diff } }, NONE)).toEqual({
      code: "approval_required",
      message: "Approve it.",
      details: { approval: "d1", diff: { ...diff, changes: [{ name: "api", settings: [{ ...setting, after: { secret: true } }] }] } },
    });
    const conflict = { code: "conflict", message: "Working State moved from revision 1 to 9", details: { revision: 9 } };
    expect(refusalForSupport(conflict, NONE)).toEqual(conflict);
  });
});

describe("a health check's output", () => {
  it("keeps the exit code and drops what the check printed", () => {
    expect(withoutCheckOutput("timed out; last check exited 127: sh: pg_isready: not found")).toBe("timed out; last check exited 127");
    expect(withoutCheckOutput("container db failed health monitoring: running (health: unhealthy); last check exited -1: a: b"))
      .toBe("container db failed health monitoring: running (health: unhealthy); last check exited -1");
    expect(withoutCheckOutput("timed out; last check exited 1")).toBe("timed out; last check exited 1");
    expect(withoutCheckOutput("timed out; last check: HTTP 503")).toBe("timed out; last check: HTTP 503");
  });
});

describe("everything support reads, with every switch off", () => {
  const VARIABLE = "VARIABLE-MARKER";
  const FILE = "FILE-MARKER";
  const CHECK = "CHECK-MARKER";
  const LOG = "LOG-MARKER";
  const MARKERS = [VARIABLE, FILE, CHECK, LOG];

  const variable = { after: VARIABLE, before: `${VARIABLE}-before`, canRestore: true, kind: "change", path: "api.env.LOG_LEVEL", row: `${API}:variables.LOG_LEVEL` };
  const unrowed = { after: VARIABLE, before: null, canRestore: true, kind: "add", path: "api.variables.LOG_LEVEL", row: null };
  const file = { after: { content: FILE, mode: "0444" }, before: null, canRestore: true, kind: "add", path: "configs.@8aa5.files.config.yml", row: "8aa5:files.config.yml" };
  const changes = [{ name: "api", settings: [variable, unrowed, file] }];
  const diff = { changes, draft_changes: changes, follow_hints: [{ row: `${API}:variables.LOG_LEVEL`, value: VARIABLE }], version: "v9" };
  const historyPreview = { revision: 1, action: "restore", version: "v9", changes, total_count: 3, overwritten: ["api.env.LOG_LEVEL"] };
  const merge = { row: `${API}:variables.LOG_LEVEL`, production: VARIABLE, lands: VARIABLE, preview: VARIABLE };
  const destinations = [{ rows: [merge, { row: "8aa5:files.config.yml", production: { content: FILE }, lands: { content: FILE }, preview: null }] }];
  const cause = [`timed out; last check exited 1: ${CHECK}`, `container db failed health monitoring: running; last check exited 127: ${CHECK}`];
  const outcome = { type: "executed", summary: { deployed: 1 }, reason: "A Service failed", cause };
  const summary = { number: 3, outcome };
  const failed = { machine_id: "m1", server: "one", started_at: 1, finished_at: 2, state: "failed", reason: "The Container failed its health check", cause, step: null, log: LOG };
  const deployment = { ...summary, nodes: [{ type: "service", id: API, name: "api", outcome: "failed", rows: [failed] }] };

  const views: ReadonlyArray<ConfigView> = [
    view({
      view: "environment",
      settings: [{ path: "api.env.LOG_LEVEL", row: `${API}:variables.LOG_LEVEL`, value: VARIABLE, default: VARIABLE }, { path: file.path, row: file.row, value: { content: FILE }, default: null }],
      values: { env: { LOG_LEVEL: VARIABLE, SHARED: { exported: true, value: VARIABLE } } },
    }),
    view({ view: "diff", ...diff }),
    view({ view: "history", revisions: [{ revision: 1, changes }] }),
    view({ view: "history_preview", ...historyPreview }),
    view({ view: "plan", changes }),
    view({ view: "deployments", deployments: [summary, { number: 2, outcome: { type: "not_executed", reason: "Refused", cause, needs_upload: [] } }] }),
    view({ view: "deployment", ...deployment }),
    view({ view: "numbered_deployment", ...deployment }),
    view({ view: "service", values: { env: { LOG_LEVEL: VARIABLE } }, changes: [variable, unrowed] }),
    view({ view: "config", files: ["config.yml"], contents: { "config.yml": FILE } }),
    view({ view: "sync", rows: [{ row: `${API}:variables.LOG_LEVEL`, from: VARIABLE, into: VARIABLE }, { row: file.row, from: { content: FILE }, into: null }] }),
    view({ view: "pull_request", environments: [{ deployment: summary, destinations }], check: { summary: `LOG_LEVEL=${VARIABLE}` } }),
    view({ view: "on_merge", destinations }),
    view({ view: "environments", environments: [{ removal: summary }] }),
  ];
  const refusals: ReadonlyArray<StoreRefusal> = [
    { code: "approval_required", message: "Approve it.", details: { approval: "d1", diff: { view: "diff", ...diff } } },
    { code: "conflict", message: "The Environment changed after this History review. Review it again", details: { diff } },
    { code: "confirmation_required", message: "This History action overwrites draft changes", details: { history_preview: historyPreview } },
  ];
  const written: ReadonlyArray<ConfigWritten> = [
    asTestDouble<ConfigWritten>()({ written: "deployment", ...summary }),
    asTestDouble<ConfigWritten>()({ written: "on_merge", destinations }),
    asTestDouble<ConfigWritten>()({ written: "batch", results: [{ written: "deployment", ...summary }] }),
    asTestDouble<ConfigWritten>()({ written: "default_environment", environments: [{ removal: summary }] }),
    asTestDouble<ConfigWritten>()({ written: "branch_setup", environments: [{ removal: summary }] }),
    asTestDouble<ConfigWritten>()({ written: "removal", removal: "removing", left: [{ deployment: summary }], waiting_on: [] }),
    asTestDouble<ConfigWritten>()({ written: "automated", admitted: [{ environment: "e1", deployment: summary }] }),
  ];
  const leaked = (shown: ConfigView | ConfigWritten | StoreRefusal) => MARKERS.filter((marker) => JSON.stringify(shown).includes(marker));

  it.each(views.map((shown) => [shown.view, shown] as const))("the %s view shows none of them", (_, shown) => {
    expect(leaked(viewForSupport(shown, NONE))).toEqual([]);
    expect(viewForSupport(shown, ALL)).toEqual(shown);
  });

  it.each(refusals.map((refusal) => [refusal.code, refusal] as const))("a %s refusal shows none of them", (_, refusal) => {
    expect(leaked(refusalForSupport(refusal, NONE))).toEqual([]);
    expect(refusalForSupport(refusal, ALL)).toEqual(refusal);
  });

  it.each(written.map((shown) => [shown.written, shown] as const))("a written %s shows none of them", (_, shown) => {
    expect(leaked(writtenForSupport(shown, NONE))).toEqual([]);
    expect(writtenForSupport(shown, ALL)).toEqual(shown);
  });

  it("still says why a Deployment failed", () => {
    const shown = JSON.stringify(viewForSupport(views.find((one) => one.view === "deployment") ?? view({}), NONE));
    expect(shown).toContain("timed out; last check exited 1\"");
    expect(shown).toContain("last check exited 127\"");
    expect(shown).toContain("The Container failed its health check");
  });

  it("every marker is in what it starts from", () => {
    const all: ReadonlyArray<JsonValue | object> = [...views, ...refusals, ...written];
    expect(MARKERS.filter((marker) => !JSON.stringify(all).includes(marker))).toEqual([]);
  });
});
