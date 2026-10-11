import { TASKS } from "./tasks";
import { outcome, type TrialResult } from "./trial";

/** One trial's grade, as the results file keeps it. */
export type Trial = {
  readonly task: string;
  readonly model: string;
  readonly failures: ReadonlyArray<string>;
  readonly toolCalls: number;
  readonly refusals: number;
};

export const trialOf = (task: string, model: string, result: TrialResult): Trial => {
  const calls = result.transcript.flatMap((turn) => turn.calls);
  return { task, model, failures: result.failures, toolCalls: calls.length, refusals: calls.filter((call) => outcome(call).refusal !== undefined).length };
};

const choose = (n: number, k: number) => Array.from({ length: k }, (_, index) => (n - index) / (index + 1)).reduce((product, factor) => product * factor, 1);

/** The chance that k trials drawn from n, c of them passing, all pass: C(c,k)/C(n,k). `null` with fewer than k trials. */
export const passHatK = (n: number, c: number, k: number) => (n < k ? null : choose(c, k) / choose(n, k));

export type Row = {
  readonly n: number;
  readonly passed: number;
  readonly pass1: number | null;
  readonly pass4: number | null;
  readonly meanToolCalls: number;
  readonly meanRefusals: number;
};

export type Summary = {
  /** Each model's row for each task it ran. */
  readonly models: Readonly<Record<string, Readonly<Record<string, Row>>>>;
  /** Tasks no model passed once, the ceiling included: a broken task or tool, not a weak model. */
  readonly quarantined: ReadonlyArray<string>;
};

const mean = (values: ReadonlyArray<number>) => values.reduce((sum, value) => sum + value, 0) / values.length;

export const summarize = (trials: ReadonlyArray<Trial>): Summary => {
  const models = Object.fromEntries([...new Set(trials.map(({ model }) => model))].map((model) => {
    const mine = trials.filter((trial) => trial.model === model);
    const rows = [...new Set(mine.map(({ task }) => task))].map((task): readonly [string, Row] => {
      const runs = mine.filter((trial) => trial.task === task);
      const passed = runs.filter(({ failures }) => failures.length === 0).length;
      return [task, {
        n: runs.length,
        passed,
        pass1: passHatK(runs.length, passed, 1),
        pass4: passHatK(runs.length, passed, 4),
        meanToolCalls: mean(runs.map(({ toolCalls }) => toolCalls)),
        meanRefusals: mean(runs.map(({ refusals }) => refusals)),
      }];
    });
    return [model, Object.fromEntries(rows)];
  }));
  const ran = [...new Set(trials.map(({ task }) => task))];
  const quarantined = ran.filter((task) => Object.values(models).every((rows) => (rows[task]?.passed ?? 0) === 0));
  return { models, quarantined };
};

const score = (value: number | null) => (value === null ? "n/a" : value.toFixed(2));

/** `summary` as Markdown: one table for the tasks, one for the held-out paraphrases, each cell `pass^1 / pass^4 (calls, refusals)`. */
export const markdown = (summary: Summary) => {
  const models = Object.keys(summary.models);
  const table = (heldOut: boolean) => {
    const tasks = TASKS.filter(({ id, tags }) => tags.includes("held-out") === heldOut && models.some((model) => summary.models[model]?.[id] !== undefined));
    return [
      `| Task | ${models.join(" | ")} |`,
      `|---|${models.map(() => "---|").join("")}`,
      ...tasks.map(({ id }) => `| ${id}${summary.quarantined.includes(id) ? " (quarantined)" : ""} | ${models.map((model) => {
        const row = summary.models[model]?.[id];
        return row === undefined ? "" : `${score(row.pass1)} / ${score(row.pass4)} (${row.meanToolCalls.toFixed(1)}, ${row.meanRefusals.toFixed(1)})`;
      }).join(" | ")} |`),
    ].join("\n");
  };
  return [
    "Each cell is pass^1 / pass^4, then mean tool calls and mean refusals per trial.",
    "",
    "## Tasks",
    "",
    table(false),
    "",
    "## Held-out paraphrases",
    "",
    table(true),
    "",
    `Quarantined: ${summary.quarantined.join(", ") || "none"}.`,
    "",
  ].join("\n");
};
