import { describe, it } from "@effect/vitest";
import { Effect } from "effect";
import { expect } from "vitest";
import type { JsonObject } from "#/db/tables";
import { ScriptedAdapter } from "#/modules/agent/scripted-adapter.server";
import { TASKS } from "./tasks";
import { runTrial } from "./trial";

/** An agent that makes the one call a task needs, then reports what came back. */
type Attempt = { readonly tool: string; readonly input: JsonObject };

const app = (env: string, service: string) => ({ project: "app", env, service });
const volume = (project: string, env: string, name: string, more: JsonObject = {}) => ({ project, env, volume: name, ...more });

/**
 * Each fleet task as an honest agent tries it on today's sidebar, and the failures that trial records. Every task
 * needs a tool the sidebar does not have yet, so each fails because that call gets no answer, and for no other reason.
 */
const TODAY = {
  D1: { tool: "service_logs", input: app("staging", "web"), failures: [] },
  D2: { tool: "service_status", input: app("production", "web"), failures: [] },
  A1: { tool: "service_restart", input: app("production", "web"), failures: [] },
  A2: { tool: "server_drain", input: { server: "web-2" }, failures: [] },
  A3: { tool: "volume_mirror", input: volume("app", "staging", "pgdata", { server: "web-2" }), failures: [] },
  A4: { tool: "volume_mirror_remove", input: volume("app", "staging", "pgdata", { server: "web-2" }), failures: [] },
  A5a: { tool: "project_remove", input: { project: "trial" }, failures: [] },
  A5b: { tool: "project_remove", input: { project: "trial", every_copy: ["production/db"] }, failures: [] },
  A6: { tool: "project_remove", input: { project: "trial" }, failures: [] },
  A7: { tool: "volume_release", input: volume("trial", "preview", "uploads"), failures: [] },
  A8: { tool: "server_upgrade", input: { server: "web-1" }, failures: [] },
  A9: { tool: "server_clean", input: { server: "web-1" }, failures: [] },
  A10: { tool: "volume_mirror", input: volume("app", "staging", "pgdata"), failures: [] },
  S5: { tool: "service_logs", input: app("staging", "web"), failures: [] },
  S6: { tool: "service_status", input: app("production", "web"), failures: [] },
  S7: { tool: "service_restart", input: app("staging", "worker"), failures: [] },
  S8: { tool: "service_restart", input: app("production", "web"), failures: [] },
  S9: { tool: "service_logs", input: app("staging", "web"), failures: [] },
} satisfies Record<string, Attempt & { readonly failures: ReadonlyArray<string> }>;

const attempt = ({ tool, input }: Attempt) => new ScriptedAdapter((messages) => messages.at(-1)?.role === "tool"
  ? { text: `I could not get an answer from ${tool}, so I don't know yet.` }
  : { calls: [{ tool, input }] });

const FLEET_TASKS = TASKS.filter((task) => task.fleet !== undefined);

describe("today's sidebar", () => {
  it("is tried on every fleet task", () => {
    expect(Object.keys(TODAY).sort()).toEqual(FLEET_TASKS.map(({ id }) => id).sort());
  });
  for (const [id, today] of Object.entries(TODAY)) {
    const task = FLEET_TASKS.find((candidate) => candidate.id === id);
    if (task === undefined) continue;
    it.live(`fails ${task.id} only for want of ${today.tool}`, () => Effect.gen(function* () {
      const member = { open: async () => task.persona.opening, reply: async () => null };
      const { failures } = yield* runTrial(task, attempt(today), member);
      expect(failures).toEqual(today.failures);
    }));
  }
});
