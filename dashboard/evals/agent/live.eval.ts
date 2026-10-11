import { existsSync, mkdirSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import { describe, it } from "@effect/vitest";
import { Effect, Exit, Result } from "effect";
import { markdown, summarize, type Trial, trialOf } from "./metrics";
import { model, MODELS } from "./models";
import { simulated } from "./simulator";
import { type Golden, Recorder, recording } from "./tape";
import { TASKS } from "./tasks";
import { runTrial, type Turn } from "./trial";

const TRIALS = Number(process.env["PLOYZ_EVAL_TRIALS"] ?? 8);
const CONCURRENCY = Number(process.env["PLOYZ_EVAL_CONCURRENCY"] ?? 12);
/** Attempts per trial in one run when its CLI breaks rather than answers. A trial still broken is left for the next run. */
const ATTEMPTS = 3;
const only = (list: string | undefined) => (name: string) => list === undefined || list.split(",").includes(name);
const RESULTS = new URL("../results/", import.meta.url);
const TRIALS_DIR = new URL("trials/", RESULTS);
const GOLDEN = new URL("./golden/", import.meta.url);

/** One finished trial as saved on disk: its grade, plus the transcript that earned it. */
type Saved = { readonly trial: Trial; readonly transcript: ReadonlyArray<Turn> };

const saved = (name: string, task: string, index: number) => new URL(`${encodeURIComponent(name)}/${task}-${index}.json`, TRIALS_DIR);

/** A CLI that died, hung or hit a usage limit says nothing about the model, so the trial is run again instead of graded. */
const BROKEN = /gave no answer within|exited \d+|was interrupted|rate.?limit|usage limit|overloaded|ENOMEM|EAGAIN/i;
const broken = (transcript: ReadonlyArray<Turn>) => transcript.flatMap((turn) => turn.errors).find((error) => BROKEN.test(error));

const read = (name: string): ReadonlyArray<Saved> => {
  const dir = new URL(`${encodeURIComponent(name)}/`, TRIALS_DIR);
  if (!existsSync(dir)) return [];
  // SAFETY: only this runner writes these files, each one a Saved record.
  return readdirSync(dir).filter((file) => file.endsWith(".json")).map((file) => JSON.parse(readFileSync(new URL(file, dir), "utf8")) as Saved);
};

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
    // Trial-major, so every model and task gains trials at the same pace and a stopped run leaves an even sample.
    const runs = Array.from({ length: TRIALS }, (_, index) => tasks.flatMap((task) => runnable.map(([name, agent]) => ({ name, agent, task, index }))))
      .flat()
      .filter(({ name, task, index }) => !existsSync(saved(name, task.id, index)));
    yield* Effect.logInfo(`${runs.length} trials to run, ${CONCURRENCY} at a time`);
    for (const [name] of runnable) mkdirSync(new URL(`${encodeURIComponent(name)}/`, TRIALS_DIR), { recursive: true });
    let done = 0;
    yield* Effect.forEach(runs, ({ name, agent, task, index }) => Effect.gen(function* () {
      for (let attempt = 1; attempt <= ATTEMPTS; attempt++) {
        const recorder = new Recorder(agent);
        const member = recording(simulated(simulator.success, task.persona));
        const exit = yield* Effect.exit(Effect.scoped(runTrial(task, recorder, member.member)));
        const why = Exit.isSuccess(exit) ? broken(exit.value.transcript) : String(Exit.isFailure(exit) ? exit.cause : "");
        if (Exit.isFailure(exit) || why !== undefined) {
          yield* Effect.logWarning(`${name} ${task.id}-${index} attempt ${attempt} broke: ${why?.slice(0, 200)}`);
          continue;
        }
        const result = exit.value;
        const record: Saved = { trial: trialOf(task.id, name, result), transcript: result.transcript };
        writeFileSync(saved(name, task.id, index), `${JSON.stringify(record, null, 2)}\n`);
        // The first pass becomes the golden and stays: parallel trials would otherwise overwrite it with whichever passed last.
        const golden = new URL(`${task.id}.json`, GOLDEN);
        if (result.failures.length === 0 && !task.tags.includes("held-out") && !existsSync(golden)) {
          const played: Golden = { task: task.id, model: name, member: member.said, calls: recorder.calls };
          writeFileSync(golden, `${JSON.stringify(played, null, 2)}\n`);
        }
        yield* Effect.logInfo(`${++done}/${runs.length} ${name} ${task.id}-${index} ${result.failures.length === 0 ? "pass" : "fail"}`);
        return;
      }
    }), { concurrency: CONCURRENCY, discard: true });
    const all = runnable.flatMap(([name]) => read(name).map(({ trial }) => trial));
    for (const [name] of runnable) {
      const summary = summarize(all.filter((trial) => trial.model === name));
      writeFileSync(new URL(`${name}.json`, RESULTS), `${JSON.stringify(summary, null, 2)}\n`);
      writeFileSync(new URL(`${name}.md`, RESULTS), markdown(summary));
    }
    const missing = runnable.length * tasks.length * TRIALS - all.filter((trial) => tasks.some(({ id }) => id === trial.task)).length;
    writeFileSync(new URL("baseline.md", RESULTS), `${markdown(summarize(all))}\nMissing trials: ${missing}.\n`);
    yield* Effect.logInfo(`${missing} trials missing\n${markdown(summarize(all))}`);
  }), 24 * 60 * 60 * 1000);
});
