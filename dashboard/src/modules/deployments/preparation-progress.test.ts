import type { MachineId } from "@ployz/sdk";
import { expect, it } from "vitest";
import { preparationProgressCollector } from "./preparation-progress";

it("turns phases and BuildKit steps into one step tree with attributed output", () => {
  let tick = 0;
  const progress = preparationProgressCollector(() => new Date(1_000 + tick++));
  expect(progress.event({ Build: { Stage: "Upload" } })).toEqual({
    progress: { phase: "build", serviceId: null, machineId: null, machineName: null, message: "Uploading source" },
    steps: [{ key: "stage:Upload", name: "Uploading source", startedAt: new Date(1_000), completedAt: null, cached: false, error: null }], output: [],
  });
  // Building has no row of its own: the BuildKit steps are the build.
  const building = progress.event({ Build: { Stage: "Building" } });
  expect(building.steps.map((step) => [step.key, step.completedAt])).toEqual([["stage:Upload", new Date(1_001)]]);
  expect(progress.event({ Build: { Step: { id: "sha256:a", name: "[sdk 1/2] RUN cargo build", started: "2026-09-22T21:09:06Z", completed: null, cached: false, error: null } } }).steps).toEqual([
    { key: "sha256:a", name: "[sdk 1/2] RUN cargo build", startedAt: new Date("2026-09-22T21:09:06Z"), completedAt: null, cached: false, error: null },
  ]);
  expect(progress.event({ Build: { StepOutput: { step: "sha256:a", stderr: true, text: "Compiling\n" } } }).output).toEqual([{ step: "sha256:a", stderr: true, text: "Compiling\n" }]);
  expect(progress.event({ Build: { Stage: "Output" } }).steps.map((step) => step.key)).toEqual(["stage:Output"]);
  expect(progress.event({ Build: { Stage: "Cleanup" } }).steps.map((step) => step.key)).toEqual(["stage:Output"]);
  expect(progress.event("Transfer").steps.map((step) => step.key)).toEqual(["transfer"]);
  expect(progress.event({ Delivered: { image: "web:1", machine_id: "m1" as MachineId } }).output).toEqual([{ step: "transfer", stderr: false, text: "Delivered web:1 to m1\n" }]);
  expect(progress.finish()).toEqual([{ key: "transfer", name: "Delivering images", startedAt: new Date(1_008), completedAt: new Date(1_009), cached: false, error: null }]);
  expect(progress.finish()).toEqual([]);
});

it("keeps builder messages in their own row, which fails when nothing else explains the failure", () => {
  const progress = preparationProgressCollector(() => new Date(5_000));
  progress.event({ Build: { Stage: "Building" } });
  const first = progress.event({ Build: { Output: Array.from(Buffer.from("ERROR: failed to solve: dockerfile parse error\n")) } });
  expect(first.steps.map((step) => step.key)).toEqual(["build-output"]);
  expect(first.output).toEqual([{ step: "build-output", stderr: false, text: "ERROR: failed to solve: dockerfile parse error\n" }]);
  expect(progress.event({ Build: { Output: Array.from(Buffer.from("more\n")) } }).steps).toEqual([]);
  expect(progress.finish("build failed", "Building").map((step) => [step.key, step.error])).toEqual([["build-output", "build failed"]]);
});

it("blames the stage the engine names, not the cleanup that followed it", () => {
  const progress = preparationProgressCollector(() => new Date(5_000));
  progress.event({ Build: { Stage: "Upload" } });
  progress.event({ Build: { Stage: "Building" } });
  progress.event({ Build: { Stage: "Cleanup" } });
  expect(progress.finish("dockerfile parse error", "Building").map((step) => [step.key, step.error])).toEqual([["stage:Building", "dockerfile parse error"]]);
  const cleanup = preparationProgressCollector(() => new Date(5_000));
  cleanup.event({ Build: { Stage: "Building" } });
  cleanup.event({ Build: { Stage: "Cleanup" } });
  expect(cleanup.finish("builder removal failed", "Cleanup").map((step) => [step.key, step.error])).toEqual([["stage:Cleanup", "builder removal failed"]]);
});

it("blames the failed BuildKit step rather than the builder output", () => {
  const progress = preparationProgressCollector(() => new Date(5_000));
  progress.event({ Build: { Stage: "Building" } });
  progress.event({ Build: { Output: Array.from(Buffer.from("WARNING: harmless\n")) } });
  progress.event({ Build: { Step: { id: "sha256:a", name: "[1/1] RUN false", started: "2026-09-22T21:09:06Z", completed: "2026-09-22T21:09:07Z", cached: false, error: "exit code: 1" } } });
  expect(progress.finish("build failed", "Building").map((step) => [step.key, step.error])).toEqual([["build-output", null]]);
});

it("preserves UTF-8 split between builder output events", () => {
  const progress = preparationProgressCollector();
  const bytes = Buffer.from("error: 🐴\n");
  const first = progress.event({ Build: { Output: Array.from(bytes.subarray(0, 9)) } });
  const second = progress.event({ Build: { Output: Array.from(bytes.subarray(9)) } });
  expect([...first.output, ...second.output].map((row) => row.text).join("")).toBe("error: 🐴\n");
});
