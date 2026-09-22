import type { PreparationEvent } from "@ployz/sdk";
import type { PreparationProgress } from "./deployment-progress";

/** A build step as the engine reports it: a BuildKit vertex or a Ployz-owned phase. */
export type BuildStepWrite = { key: string; name: string; startedAt: Date | null; completedAt: Date | null; cached: boolean; error: string | null };
export type BuildOutputWrite = { step: string; stderr: boolean; text: string };
export type PreparationWrites = { progress: PreparationProgress | null; steps: BuildStepWrite[]; output: BuildOutputWrite[] };

/** Builder messages outside any BuildKit step, such as a Dockerfile parse error. */
export const BUILD_OUTPUT_KEY = "build-output";

const stageNames = {
  Admission: "Waiting for the builder", Queued: "Queued", Upload: "Uploading source", Preparation: "Preparing the builder",
  Building: "Building", Output: "Loading images", Cleanup: "Cleaning up",
} satisfies Record<string, string>;
const stageName = (stage: string) => Object.entries(stageNames).find(([key]) => key === stage)?.[1] ?? stage;
// Building is the BuildKit steps themselves; Cleanup is noise unless it fails.
const silentStages = new Set(["Building", "Cleanup"]);

/**
 * Folds one attempt's preparation events into progress plus build-log writes.
 * Ployz phases become steps like BuildKit's own, so the log is one tree.
 * Provider errors and rejection dumps are never logs.
 */
export function preparationProgressCollector(now: () => Date = () => new Date()) {
  const decoder = new TextDecoder();
  let current: PreparationProgress = { phase: "selection", serviceId: null, machineId: null, machineName: null, message: null };
  let phase: { key: string; name: string; startedAt: Date; silent: boolean } | null = null;
  const phaseStarts = new Map<string, { name: string; startedAt: Date }>();
  let builderOutput: Date | null = null;
  let stepFailed = false;
  const end = (error: string | null = null): BuildStepWrite[] => {
    if (!phase) return [];
    const step = { key: phase.key, name: phase.name, startedAt: phase.startedAt, completedAt: now(), cached: false, error };
    const silent = phase.silent && !error;
    phase = null;
    return silent ? [] : [step];
  };
  const begin = (key: string, name: string, silent = false): BuildStepWrite[] => {
    const steps = end();
    phase = { key, name, startedAt: now(), silent };
    phaseStarts.set(key, { name, startedAt: phase.startedAt });
    return silent ? steps : [...steps, { key, name, startedAt: phase.startedAt, completedAt: null, cached: false, error: null }];
  };
  const none = (): PreparationWrites => ({ progress: null, steps: [], output: [] });
  const builderLine = (text: string): PreparationWrites => {
    if (!text) return none();
    const steps: BuildStepWrite[] = [];
    if (!builderOutput) {
      builderOutput = now();
      steps.push({ key: BUILD_OUTPUT_KEY, name: "Build output", startedAt: builderOutput, completedAt: null, cached: false, error: null });
    }
    return { progress: null, steps, output: [{ step: BUILD_OUTPUT_KEY, stderr: false, text }] };
  };
  return {
    current: () => current,
    event(event: PreparationEvent): PreparationWrites {
      if (event === "Transfer") {
        current = { ...current, phase: "transfer", message: "Delivering images" };
        return { progress: current, steps: begin("transfer", "Delivering images"), output: [] };
      }
      if ("Selected" in event) {
        current = { ...current, machineId: event.Selected.machine.id, machineName: event.Selected.machine.name, message: "Builder selected" };
        return { progress: current, steps: [], output: [] };
      }
      if ("Delivered" in event) {
        return { progress: null, steps: [], output: phase ? [{ step: phase.key, stderr: false, text: `Delivered ${event.Delivered.image} to ${event.Delivered.machine_id}\n` }] : [] };
      }
      if ("Platforms" in event) return none();
      const build = event.Build;
      if ("Stage" in build) {
        const name = stageName(build.Stage);
        current = { ...current, phase: "build", message: name };
        return { progress: current, steps: begin(`stage:${build.Stage}`, name, silentStages.has(build.Stage)), output: [] };
      }
      if ("Output" in build) return builderLine(decoder.decode(Uint8Array.from(build.Output), { stream: true }));
      if ("Step" in build) {
        const step = build.Step;
        stepFailed ||= step.error !== null;
        return { progress: null, output: [], steps: [{ key: step.id, name: step.name, startedAt: step.started ? new Date(step.started) : null, completedAt: step.completed ? new Date(step.completed) : null, cached: step.cached, error: step.error }] };
      }
      if ("StepOutput" in build) return { progress: null, steps: [], output: [{ step: build.StepOutput.step, stderr: build.StepOutput.stderr, text: build.StepOutput.text }] };
      return none();
    },
    /**
     * Close open steps once preparation ends. The engine names the failed
     * stage, which may have ended before cleanup ran. A failure no BuildKit
     * step explains lands on the builder's own output row when there is one,
     * otherwise on the failed stage's row.
     */
    finish(error: string | null = null, stage: string | null = null): BuildStepWrite[] {
      const culprit = stage ? `stage:${stage}` : phase?.key ?? null;
      const explained = stepFailed || builderOutput !== null;
      const steps = end(phase && phase.key === culprit && !(phase.silent && explained) ? error : null);
      if (error && !stepFailed) {
        if (builderOutput) {
          steps.push({ key: BUILD_OUTPUT_KEY, name: "Build output", startedAt: builderOutput, completedAt: now(), cached: false, error });
        } else if (culprit && !steps.some((step) => step.key === culprit)) {
          const start = phaseStarts.get(culprit);
          if (start) steps.push({ key: culprit, ...start, completedAt: now(), cached: false, error });
        }
      }
      if (builderOutput) {
        if (!steps.some((step) => step.key === BUILD_OUTPUT_KEY)) steps.push({ key: BUILD_OUTPUT_KEY, name: "Build output", startedAt: builderOutput, completedAt: now(), cached: false, error: null });
        builderOutput = null;
      }
      return steps;
    },
  };
}
