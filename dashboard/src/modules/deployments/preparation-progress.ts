import type { PreparationEvent } from "@ployz/sdk";
import { Schema } from "effect";
import type { PreparationProgress } from "./deployment-progress";

/** A build step as the engine reports it: a BuildKit vertex or a Ployz-owned phase. */
export type BuildStepWrite = { build: number; key: string; name: string; startedAt: Date | null; completedAt: Date | null; cached: boolean; error: string | null };
export type BuildOutputWrite = { build: number; step: string; stderr: boolean; text: string };
export type PreparationWrites = { progress: PreparationProgress | null; steps: BuildStepWrite[]; output: BuildOutputWrite[] };

/**
 * Where a collector left off, as JSON: enough to fold a later batch of the same build's events as
 * if it had never stopped. A GitHub runner reports its Build Steps in batches.
 */
export const collectorCheckpointSchema = Schema.Struct({
  build: Schema.Number,
  open: Schema.NullOr(Schema.String),
  stepFailed: Schema.Boolean,
  rows: Schema.Array(Schema.Struct({
    build: Schema.Number, key: Schema.String, name: Schema.String,
    startedAt: Schema.NullOr(Schema.String), completedAt: Schema.NullOr(Schema.String),
    cached: Schema.Boolean, error: Schema.NullOr(Schema.String),
  })),
});
export type CollectorCheckpoint = typeof collectorCheckpointSchema.Type;

/** Builder messages outside any BuildKit step, such as a Dockerfile parse error. */
export const BUILD_OUTPUT_KEY = "build-output";

const stageNames = new Map([
  ["Admission", "Waiting for the builder"], ["Queued", "Queued"], ["Upload", "Uploading source"], ["Preparation", "Preparing the builder"],
  ["Building", "Building"], ["Output", "Loading images"], ["Cleanup", "Cleaning up"],
]);
const stageName = (stage: string) => stageNames.get(stage) ?? stage;
export const BUILDING_KEY = "stage:Building";
/** Attempt-wide rows the engine files under the last build run: image cleanup and delivery. */
export const CLEANUP_KEY = "stage:Cleanup";
export const TRANSFER_KEY = "transfer";

/**
 * Which step a failed attempt is pinned on: the failed BuildKit step already
 * says it; builder messages explain a failure the engine pins on Building;
 * otherwise the stage the engine names, falling back to the open phase.
 */
function blameFor(input: { error: string | null; stage: string | null; stepFailed: boolean; hasBuilderOutput: boolean; openKey: string | null }): string | null {
  if (!input.error || input.stepFailed) return null;
  if (input.hasBuilderOutput && input.stage === "Building") return BUILD_OUTPUT_KEY;
  return input.stage ? stageKey(input.stage) : input.openKey;
}

const stageKey = (stage: string) => `stage:${stage}`;
const stageOfKey = (key: string) => key.replace(/^stage:/, "");

const rowId = (build: number, key: string) => `${build}:${key}`;

/**
 * Folds one attempt's preparation events into progress plus build-log writes.
 * Ployz phases become steps like BuildKit's own, so the log is one tree. An
 * attempt runs BuildKit once per image (and per platform); every run gets its
 * own ordinal so identical steps never collide, and its Building row names the
 * image, attributing the run's steps and output to that Image Build.
 * Provider errors and rejection dumps are never logs.
 */
export function preparationProgressCollector(now: () => Date = () => new Date(), resume?: CollectorCheckpoint) {
  // ponytail: a multibyte character split across two resumed batches decodes as replacement characters.
  const decoder = new TextDecoder();
  let current: PreparationProgress = { phase: "selection", serviceId: null, machineId: null, machineName: null, message: null };
  const date = (value: string | null) => value === null ? null : new Date(value);
  /** Ployz-owned rows by build and key, mutated in place; the open phase is the one without a completion. */
  const rows = new Map<string, BuildStepWrite>((resume?.rows ?? []).map((row) =>
    [rowId(row.build, row.key), { ...row, startedAt: date(row.startedAt), completedAt: date(row.completedAt) }]));
  let open: string | null = resume?.open ?? null;
  let build = resume?.build ?? 0;
  let stepFailed = resume?.stepFailed ?? false;
  let finished = false;
  const create = (key: string, name: string): BuildStepWrite => {
    const row = { build, key, name, startedAt: now(), completedAt: null, cached: false, error: null };
    rows.set(rowId(build, key), row);
    return row;
  };
  const close = (id: string | null): BuildStepWrite[] => {
    const row = id === null ? undefined : rows.get(id);
    if (!row) return [];
    row.completedAt = now();
    return [{ ...row }];
  };
  const begin = (key: string, name: string): BuildStepWrite[] => {
    if (open === rowId(build, key)) return [];
    const closed = close(open);
    open = rowId(build, key);
    return [...closed, { ...create(key, name) }];
  };
  const none = (): PreparationWrites => ({ progress: null, steps: [], output: [] });
  const builderLine = (text: string): PreparationWrites => {
    if (!text) return none();
    const steps = rows.has(rowId(build, BUILD_OUTPUT_KEY)) ? [] : [{ ...create(BUILD_OUTPUT_KEY, "Build output") }];
    return { progress: null, steps, output: [{ build, step: BUILD_OUTPUT_KEY, stderr: false, text }] };
  };
  return {
    current: () => current,
    checkpoint: (): CollectorCheckpoint => ({
      build, open, stepFailed,
      rows: [...rows.values()].map((row) => ({ ...row, startedAt: row.startedAt?.toISOString() ?? null, completedAt: row.completedAt?.toISOString() ?? null })),
    }),
    event(event: PreparationEvent): PreparationWrites {
      if (event === "Transfer") {
        current = { ...current, phase: "transfer", message: "Delivering images" };
        return { progress: current, steps: begin(TRANSFER_KEY, "Delivering images"), output: [] };
      }
      if ("Selected" in event) {
        current = { ...current, machineId: event.Selected.machine.id, machineName: event.Selected.machine.name, message: "Builder selected" };
        return { progress: current, steps: [], output: [] };
      }
      if ("Delivered" in event) {
        const row = open === null ? undefined : rows.get(open);
        return { progress: null, steps: [], output: row ? [{ build: row.build, step: row.key, stderr: false, text: `Delivered ${event.Delivered.image} to ${event.Delivered.machine_id}\n` }] : [] };
      }
      if ("Platforms" in event) return none();
      const build_ = event.Build;
      if ("Stage" in build_) {
        const name = stageName(build_.Stage);
        current = { ...current, phase: "build", message: name };
        if (build_.Stage === "Building") build += 1;
        return { progress: current, steps: begin(stageKey(build_.Stage), name), output: [] };
      }
      if ("Target" in build_) {
        // Each run builds one image and the engine names it as the run starts,
        // so the run is that image's Image Build: its heading row carries the image.
        const heading = rows.get(rowId(build, BUILDING_KEY));
        if (!heading || heading.name === build_.Target.name) return none();
        heading.name = build_.Target.name;
        return { progress: null, steps: [{ ...heading }], output: [] };
      }
      if ("Output" in build_) return builderLine(decoder.decode(Uint8Array.from(build_.Output), { stream: true }));
      if ("Step" in build_) {
        const step = build_.Step;
        stepFailed ||= step.error !== null;
        return { progress: null, output: [], steps: [{ build, key: step.id, name: step.name, startedAt: step.started ? new Date(step.started) : null, completedAt: step.completed ? new Date(step.completed) : null, cached: step.cached, error: step.error }] };
      }
      if ("StepOutput" in build_) return { progress: null, steps: [], output: [{ build, step: build_.StepOutput.step, stderr: build_.StepOutput.stderr, text: build_.StepOutput.text }] };
      return none();
    },
    /**
     * Close the open rows once preparation ends and pin a failure on one row.
     * The engine names the failed stage, which may have ended before cleanup
     * ran; a blamed row that never reported is created now.
     */
    finish(error: string | null = null, stage: string | null = null): BuildStepWrite[] {
      if (finished) return [];
      finished = true;
      const openRow = open === null ? undefined : rows.get(open);
      const blame = blameFor({ error, stage, stepFailed, hasBuilderOutput: rows.has(rowId(build, BUILD_OUTPUT_KEY)), openKey: openRow?.key ?? null });
      const blamed = blame === null ? null : rowId(build, blame);
      if (blame && blamed) {
        const row = rows.get(blamed) ?? create(blame, stageName(stageOfKey(blame)));
        row.error = error;
      }
      return [...rows].flatMap(([id, row]) => {
        if (row.completedAt !== null && id !== blamed) return [];
        row.completedAt ??= now();
        return [{ ...row }];
      });
    },
  };
}
