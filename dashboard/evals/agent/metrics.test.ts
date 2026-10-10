import { describe, expect, it } from "vitest";
import { markdown, passHatK, summarize, type Trial } from "./metrics";

const trials = (model: string, task: string, passed: number, n: number): ReadonlyArray<Trial> =>
  Array.from({ length: n }, (_, index) => ({ task, model, failures: index < passed ? [] : ["wrong"], toolCalls: 2, refusals: index % 2 }));

describe("passHatK", () => {
  it("is C(c,k)/C(n,k)", () => {
    expect(passHatK(8, 8, 4)).toBe(1);
    expect(passHatK(8, 6, 4)).toBeCloseTo(15 / 70);
    expect(passHatK(8, 3, 4)).toBe(0);
    expect(passHatK(8, 6, 1)).toBe(0.75);
  });

  it("has no value with fewer trials than k", () => {
    expect(passHatK(3, 3, 4)).toBeNull();
  });
});

describe("summarize", () => {
  const summary = summarize([...trials("haiku", "T1", 0, 8), ...trials("haiku", "T2", 6, 8), ...trials("sonnet", "T1", 0, 8), ...trials("sonnet", "T2", 8, 8)]);

  it("scores each model's task with means per trial", () => {
    expect(summary.models["haiku"]?.["T2"]).toEqual({ n: 8, passed: 6, pass1: 0.75, pass4: 15 / 70, meanToolCalls: 2, meanRefusals: 0.5 });
  });

  it("quarantines only a task every model failed", () => {
    expect(summary.quarantined).toEqual(["T1"]);
  });

  it("writes one row per task in the Markdown table", () => {
    expect(markdown(summary)).toContain("| T1 (quarantined) | 0.00 / 0.00 (2.0, 0.5) | 0.00 / 0.00 (2.0, 0.5) |");
    expect(markdown(summary)).toContain("| T2 | 0.75 / 0.21 (2.0, 0.5) | 1.00 / 1.00 (2.0, 0.5) |");
  });
});
