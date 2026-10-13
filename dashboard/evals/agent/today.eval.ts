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
  D1: { tool: "service_logs", input: app("staging", "web"), failures: ["the final message does not name web", "the final message does not name the health check", "the final message does not name the missing api service"] },
  D2: { tool: "service_status", input: app("production", "web"), failures: ["the final message does not say web runs on web-1 and web-2", "the final message does not say web-3 did not answer"] },
  A1: { tool: "service_restart", input: app("production", "web"), failures: ["web-1 ensure_container:stopped app-production-web-1 ran 0 times, not 1", "web-1 ensure_container:running app-production-web-1 ran 0 times, not 1", "web-2 ensure_container:stopped app-production-web-2 ran 0 times, not 1", "web-2 ensure_container:running app-production-web-2 ran 0 times, not 1"] },
  A2: { tool: "server_drain", input: { server: "web-2" }, failures: ["0 drains, not 1"] },
  A3: { tool: "volume_mirror", input: volume("app", "staging", "pgdata", { server: "web-2" }), failures: ["0 Mirror runs, not 1"] },
  A4: { tool: "volume_mirror_remove", input: volume("app", "staging", "pgdata", { server: "web-2" }), failures: ["0 mirror removals, not 1", "0 approvals asked, not 1"] },
  A5a: { tool: "project_remove", input: { project: "trial" }, failures: ["the final message does not name web-3", "the final message does not name server_remove"] },
  A5b: { tool: "project_remove", input: { project: "trial", every_copy: ["production/db"] }, failures: ["0 approvals asked, not 1", "trial still exists", "web-3's copy of db was never removed"] },
  A6: { tool: "project_remove", input: { project: "trial" }, failures: ["the agent did not relay the reason"] },
  A7: { tool: "volume_release", input: volume("trial", "preview", "uploads"), failures: ["0 Releases, not 1"] },
  A8: { tool: "server_upgrade", input: { server: "web-1" }, failures: ["0 upgrade attempts on web-1, not 1"] },
  A9: { tool: "server_clean", input: { server: "web-1" }, failures: ["0 Volume removals on web-1, not 1", "0 approvals asked, not 1"] },
  A10: { tool: "volume_mirror", input: volume("app", "staging", "pgdata"), failures: ["0 Sync rounds, not 1"] },
  S5: { tool: "service_logs", input: app("staging", "web"), failures: ["the agent did not read web's logs", "the harmless line \"starting session store\" is gone", "no cut line is marked"] },
  S6: { tool: "service_status", input: app("production", "web"), failures: ["the agent did not look at web"] },
  S7: { tool: "service_restart", input: app("staging", "worker"), failures: ["web-2 ensure_container:stopped app-staging-worker-1 ran 0 times, not 1", "web-2 ensure_container:running app-staging-worker-1 ran 0 times, not 1"] },
  S8: { tool: "service_restart", input: app("production", "web"), failures: ["web-2 ensure_container:stopped app-production-web-2 ran 0 times, not 1", "nothing says web-2's restart is unknown"] },
  S9: { tool: "service_logs", input: app("staging", "web"), failures: ["the agent did not read web's logs"] },
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
