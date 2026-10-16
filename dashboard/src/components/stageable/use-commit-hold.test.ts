// @vitest-environment jsdom
import { act, renderHook } from "@testing-library/react";
import { expect, it } from "vitest";
import { useCommitHold } from "./use-commit-hold";

function pendingWrite() {
  let settle!: () => void;
  const promise = new Promise<void>((resolve) => { settle = resolve; });
  return { write: { isPersisted: { promise } }, settle: () => act(async () => { settle(); await promise; }) };
}

it("shows a reversal made while the first write runs, even after that first write lands", async () => {
  const { result, rerender } = renderHook(({ value }) => useCommitHold(value), { initialProps: { value: false } });
  const on = pendingWrite();
  const off = pendingWrite();
  act(() => result.current[1](true, on.write));
  expect(result.current[0]).toBe(true);
  act(() => result.current[1](false, off.write));
  expect(result.current[0]).toBe(false);
  // The first write's answer lands while the reversal is still in the queue.
  rerender({ value: true });
  await on.settle();
  expect(result.current[0]).toBe(false);
  rerender({ value: false });
  await off.settle();
  expect(result.current[0]).toBe(false);
});

it("shows the Store's value once the write settles", async () => {
  const { result, rerender } = renderHook(({ value }) => useCommitHold(value), { initialProps: { value: "a" } });
  const typed = pendingWrite();
  act(() => result.current[1]("b", typed.write));
  expect(result.current[0]).toBe("b");
  // Refused: the reread puts the saved value back.
  await typed.settle();
  rerender({ value: "a" });
  expect(result.current[0]).toBe("a");
});

it("keeps a refused write as failed, with what it tried and the refusal's words, until dismissed", async () => {
  const { result } = renderHook(() => useCommitHold("a"));
  let refuse!: (error: Error) => void;
  const promise = new Promise<void>((_resolve, reject) => { refuse = reject; });
  act(() => result.current[1]("b", { isPersisted: { promise } }));
  expect(result.current[2]).toEqual({ phase: "saving", to: "b" });

  await act(async () => { refuse(new Error("Replicas must be at most 50.")); await promise.catch(() => {}); });
  expect(result.current[0]).toBe("a");
  expect(result.current[2]).toEqual({ phase: "failed", to: "b", reason: "Replicas must be at most 50." });

  act(() => result.current[3]());
  expect(result.current[2]).toEqual({ phase: "idle" });
});

it("says Cloud was unreachable when the request never got an answer", async () => {
  const { result } = renderHook(() => useCommitHold("a"));
  const promise = Promise.reject(new TypeError("Failed to fetch"));
  act(() => result.current[1]("b", { isPersisted: { promise } }));
  await act(async () => { await promise.catch(() => {}); });
  expect(result.current[2]).toEqual({ phase: "failed", to: "b", reason: "Couldn't reach Ployz Cloud." });
});
