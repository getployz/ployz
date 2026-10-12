import type { ConfigView } from "@ployz/sdk";
import { describe, expect, it } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import type { SupportShows } from "./support-access";
import { holds, refusalForSupport, viewForSupport } from "./support-view";

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
