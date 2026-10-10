import { mkdirSync, writeFileSync } from "node:fs";
import { describe, it } from "@effect/vitest";
import { Effect, Result } from "effect";
import { markdown, summarize, trialOf } from "./metrics";
import { model, MODELS } from "./models";
import { simulated } from "./simulator";
import { type Golden, Recorder, recording } from "./tape";
import { TASKS } from "./tasks";
import { runTrial } from "./trial";

const TRIALS = Number(process.env["PLOYZ_EVAL_TRIALS"] ?? 8);
const CONCURRENCY = Number(process.env["PLOYZ_EVAL_CONCURRENCY"] ?? 4);
const only = (list: string | undefined) => (name: string) => list === undefined || list.split(",").includes(name);
const RESULTS = new URL("../results/", import.meta.url);
const GOLDEN = new URL("./golden/", import.meta.url);

describe.skipIf(process.env["PLOYZ_EVAL_LIVE"] !== "1")("live agent eval", () => {
  it.live("runs every pinned model on every task", (context) => Effect.gen(function* () {
    const simulator = yield* model(MODELS.simulator);
    if (Result.isFailure(simulator)) return context.skip(`the simulated member can't run: ${simulator.failure}`);
    const agents = yield* Effect.forEach(MODELS.agents.filter(only(process.env["PLOYZ_EVAL_MODELS"])), (name) =>
      model(name).pipe(Effect.map((agent) => [name, agent] as const)));
    for (const [name, agent] of agents) if (Result.isFailure(agent)) yield* Effect.logWarning(`skipping ${name}: ${agent.failure}`);
    const runnable = agents.flatMap(([name, agent]) => (Result.isSuccess(agent) ? [[name, agent.success] as const] : []));
    if (runnable.length === 0) return context.skip("no pinned agent model can run here");
    const tasks = TASKS.filter(({ id }) => only(process.env["PLOYZ_EVAL_TASKS"])(id));
    const runs = runnable.flatMap(([name, agent]) => tasks.flatMap((task) => Array.from({ length: TRIALS }, () => ({ name, agent, task }))));
    let failures = 0;
    const trials = yield* Effect.forEach(runs, ({ name, agent, task }) => Effect.gen(function* () {
      const recorder = new Recorder(agent);
      const member = recording(simulated(simulator.success, task.persona));
      const result = yield* Effect.scoped(runTrial(task, recorder, member.member));
      if (result.failures.length > 0) {
        const failed = new URL(`failed/${name}/`, RESULTS);
        mkdirSync(failed, { recursive: true });
        writeFileSync(new URL(`${task.id}-${++failures}.json`, failed), `${JSON.stringify({ failures: result.failures, transcript: result.transcript }, null, 2)}\n`);
      }
      if (result.failures.length === 0 && !task.tags.includes("held-out")) {
        const golden: Golden = { task: task.id, model: name, member: member.said, calls: recorder.calls };
        writeFileSync(new URL(`${task.id}.json`, GOLDEN), `${JSON.stringify(golden, null, 2)}\n`);
      }
      return trialOf(task.id, name, result);
    }), { concurrency: CONCURRENCY });
    const summary = summarize(trials);
    mkdirSync(RESULTS, { recursive: true });
    const name = runnable.map(([agent]) => agent).join("+");
    writeFileSync(new URL(`${name}.json`, RESULTS), `${JSON.stringify({ trials, summary }, null, 2)}\n`);
    writeFileSync(new URL(`${name}.md`, RESULTS), markdown(summary));
    yield* Effect.logInfo(markdown(summary));
  }), 24 * 60 * 60 * 1000);
});
