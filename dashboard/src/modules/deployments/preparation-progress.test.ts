import type { MachineId } from "@ployz/sdk";
import { expect, it } from "vitest";
import { runtimeWatchMachineFixture } from "#/modules/runtime/runtime-watch-frame.test-fixture";
import { preparationProgressCollector } from "./preparation-progress";

const keys = (steps: readonly { build: number; key: string; error: string | null }[]) => steps.map((step) => [step.build, step.key, step.error]);

it("turns phases and BuildKit steps into one step tree with attributed output", () => {
  let tick = 0;
  const progress = preparationProgressCollector(() => new Date(1_000 + tick++));
  expect(progress.event({ Build: { Stage: "Upload" } })).toEqual({
    progress: { phase: "build", serviceId: null, machineId: null, machineName: null, message: "Uploading source" },
    steps: [{ build: 0, key: "stage:Upload", name: "Uploading source", startedAt: new Date(1_000), completedAt: null, cached: false, error: null }], output: [],
  });
  // Building heads each image's own BuildKit run; the engine then names that image.
  expect(keys(progress.event({ Build: { Stage: "Building" } }).steps)).toEqual([[0, "stage:Upload", null], [1, "stage:Building", null]]);
  expect(progress.event({ Build: { Target: { name: "web", outcome: "Unknown" } } }).steps.map((step) => [step.build, step.key, step.name])).toEqual([[1, "stage:Building", "web"]]);
  expect(progress.event({ Build: { Step: { id: "sha256:a", name: "[sdk 1/2] RUN cargo build", started: "2026-09-22T21:09:06Z", completed: null, cached: false, error: null } } }).steps).toEqual([
    { build: 1, key: "sha256:a", name: "[sdk 1/2] RUN cargo build", startedAt: new Date("2026-09-22T21:09:06Z"), completedAt: null, cached: false, error: null },
  ]);
  expect(progress.event({ Build: { StepOutput: { step: "sha256:a", stderr: true, text: "Compiling\n" } } }).output).toEqual([{ build: 1, step: "sha256:a", stderr: true, text: "Compiling\n" }]);
  expect(keys(progress.event({ Build: { Stage: "Output" } }).steps)).toEqual([[1, "stage:Building", null], [1, "stage:Output", null]]);
  // The run's result names the same image, so its Image Build is unchanged.
  expect(progress.event({ Build: { Target: { name: "web", outcome: "Unknown" } } }).steps).toEqual([]);
  // The next image's run gets its own ordinal, so a shared step shows again, cached, under that image.
  expect(keys(progress.event({ Build: { Stage: "Building" } }).steps)).toEqual([[1, "stage:Output", null], [2, "stage:Building", null]]);
  expect(progress.event({ Build: { Target: { name: "api", outcome: "Unknown" } } }).steps.map((step) => [step.build, step.key, step.name])).toEqual([[2, "stage:Building", "api"]]);
  expect(progress.event({ Build: { Step: { id: "sha256:a", name: "[sdk 1/2] RUN cargo build", started: "2026-09-22T21:10:00Z", completed: "2026-09-22T21:10:00Z", cached: true, error: null } } }).steps.map((step) => [step.build, step.cached])).toEqual([[2, true]]);
  expect(progress.event({ Build: { StepOutput: { step: "sha256:a", stderr: false, text: "cached\n" } } }).output.map((row) => row.build)).toEqual([2]);
  expect(keys(progress.event({ Build: { Stage: "Cleanup" } }).steps)).toEqual([[2, "stage:Building", null], [2, "stage:Cleanup", null]]);
  expect(keys(progress.event("Transfer").steps)).toEqual([[2, "stage:Cleanup", null], [2, "transfer", null]]);
  const delivered = progress.event({ Delivered: { image: "web:1", service: "web", machine_id: "m1" as MachineId } });
  expect(delivered.output).toEqual([{ build: 2, step: "transfer", stderr: false, text: "Delivered web:1 to m1\n" }]);
  expect(keys(progress.finish())).toEqual([[2, "transfer", null]]);
  expect(progress.finish()).toEqual([]);
});

it("says a waited-for build slot plainly and files the push's lines under Pushing image", () => {
  const progress = preparationProgressCollector(() => new Date(5_000));
  expect(progress.event({ Selected: { machine: runtimeWatchMachineFixture("m1", "hel-1"), reason: { kind: "spread" }, rejections: [] } }).steps).toEqual([]);
  expect(progress.event({ Build: { Stage: "Queued" } }).steps.map((row) => row.name)).toEqual(["Waiting for a free build slot"]);
  progress.event({ Build: { Stage: "Building" } });
  expect(progress.event({ Build: { Stage: "Push" } }).steps.map((row) => row.name)).toEqual(["Building", "Pushing image"]);
  expect(progress.event({ Build: { Output: Array.from(Buffer.from("5f70bf18a086: Pushed\n")) } })).toMatchObject({
    steps: [], output: [{ build: 1, step: "stage:Push", text: "5f70bf18a086: Pushed\n" }],
  });
});

it("tells the deploy log, not the build, which Machines each image goes to", () => {
  const progress = preparationProgressCollector(() => new Date(5_000), undefined, (name) => name === "web" ? "service-web" : null);
  progress.event("Transfer");
  expect(progress.event({ Sending: { service: "web", machines: ["hel-2", "hel-3"] } })).toMatchObject({
    steps: [], progress: { phase: "transfer", serviceId: "service-web", message: "Sending image to hel-2, hel-3" },
  });
  expect(progress.event({ Delivered: { image: "web:1", service: "web", machine_id: "m1" as MachineId } }).progress).toMatchObject({ serviceId: "service-web", message: "Image sent" });
  // One line per image, however many Machines received it.
  expect(progress.event({ Delivered: { image: "web:1", service: "web", machine_id: "m2" as MachineId } }).progress).toBeNull();
  // The Service stays out of whatever phase comes next.
  expect(progress.current().serviceId).toBeNull();
});

it("keeps builder messages in their own row, which fails when the engine blames the build", () => {
  const progress = preparationProgressCollector(() => new Date(5_000));
  progress.event({ Build: { Stage: "Building" } });
  const first = progress.event({ Build: { Output: Array.from(Buffer.from("ERROR: failed to solve: dockerfile parse error\n")) } });
  expect(keys(first.steps)).toEqual([[1, "build-output", null]]);
  expect(first.output).toEqual([{ build: 1, step: "build-output", stderr: false, text: "ERROR: failed to solve: dockerfile parse error\n" }]);
  expect(progress.event({ Build: { Output: Array.from(Buffer.from("more\n")) } }).steps).toEqual([]);
  expect(keys(progress.finish("build failed", "Building"))).toEqual([[1, "stage:Building", null], [1, "build-output", "build failed"]]);
});

it("pins a failure with no named stage on the open phase, not on harmless builder output", () => {
  const progress = preparationProgressCollector(() => new Date(5_000));
  progress.event({ Build: { Stage: "Building" } });
  progress.event({ Build: { Output: Array.from(Buffer.from("WARNING: harmless\n")) } });
  expect(keys(progress.finish("stream closed"))).toEqual([[1, "stage:Building", "stream closed"], [1, "build-output", null]]);
});

it("creates the blamed stage's row even when that stage never reported", () => {
  const progress = preparationProgressCollector(() => new Date(5_000));
  expect(progress.finish("selection failed", "Selection")).toEqual([{ build: 0, key: "stage:Selection", name: "Selection", startedAt: new Date(5_000), completedAt: new Date(5_000), cached: false, error: "selection failed" }]);
});

it("blames the stage the engine names, not the cleanup that followed it", () => {
  const progress = preparationProgressCollector(() => new Date(5_000));
  progress.event({ Build: { Stage: "Upload" } });
  progress.event({ Build: { Stage: "Building" } });
  progress.event({ Build: { Stage: "Cleanup" } });
  expect(keys(progress.finish("dockerfile parse error", "Building"))).toEqual(expect.arrayContaining([[1, "stage:Cleanup", null], [1, "stage:Building", "dockerfile parse error"]]));
  const cleanup = preparationProgressCollector(() => new Date(5_000));
  cleanup.event({ Build: { Stage: "Building" } });
  cleanup.event({ Build: { Stage: "Cleanup" } });
  expect(keys(cleanup.finish("builder removal failed", "Cleanup"))).toEqual([[1, "stage:Cleanup", "builder removal failed"]]);
});

it("blames the failed BuildKit step rather than the builder output", () => {
  const progress = preparationProgressCollector(() => new Date(5_000));
  progress.event({ Build: { Stage: "Building" } });
  progress.event({ Build: { Output: Array.from(Buffer.from("WARNING: harmless\n")) } });
  progress.event({ Build: { Step: { id: "sha256:a", name: "[1/1] RUN false", started: "2026-09-22T21:09:06Z", completed: "2026-09-22T21:09:07Z", cached: false, error: "exit code: 1" } } });
  expect(keys(progress.finish("build failed", "Building"))).toEqual([[1, "stage:Building", null], [1, "build-output", null]]);
});

it("preserves UTF-8 split between builder output events", () => {
  const progress = preparationProgressCollector();
  const bytes = Buffer.from("error: 🐴\n");
  const first = progress.event({ Build: { Output: Array.from(bytes.subarray(0, 9)) } });
  const second = progress.event({ Build: { Output: Array.from(bytes.subarray(9)) } });
  expect([...first.output, ...second.output].map((row) => row.text).join("")).toBe("error: 🐴\n");
});

it("closes output from every build run while blaming only the final run", () => {
  for (const error of [null, "build failed"]) {
    const progress = preparationProgressCollector(() => new Date(5_000));
    for (let build = 1; build <= 2; build++) {
      progress.event({ Build: { Stage: "Building" } });
      progress.event({ Build: { Output: Array.from(Buffer.from("WARNING: build output\n")) } });
    }
    const output = progress.finish(error, error ? "Building" : null).filter((row) => row.key === "build-output");
    expect(output.map(({ build, completedAt, error }) => ({ build, completedAt, error }))).toEqual([
      { build: 1, completedAt: new Date(5_000), error: null },
      { build: 2, completedAt: new Date(5_000), error },
    ]);
    expect(progress.finish()).toEqual([]);
  }
});

it("keeps repeated stages open with their original start time", () => {
  let time = 1_000;
  const collector = preparationProgressCollector(() => new Date(time));
  collector.event({ Build: { Stage: "Upload" } });
  time = 2_000;
  expect(collector.event({ Build: { Stage: "Upload" } }).steps).toEqual([]);
  expect(collector.event({ Build: { Stage: "Preparation" } }).steps[0]).toMatchObject({
    key: "stage:Upload", startedAt: new Date(1_000), completedAt: new Date(2_000),
  });
  collector.event({ Build: { Stage: "Building" } });
  expect(collector.event({ Build: { Stage: "Building" } }).steps.map((row) => row.build)).toEqual([1, 2]);
});
