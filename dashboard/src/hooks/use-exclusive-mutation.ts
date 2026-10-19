import { type DefaultError, useMutation, type UseMutationOptions } from "@tanstack/react-query";
import { useRef, useState } from "react";

export const STALL_MS = 30_000;

/**
 * A mutation that ignores `mutate` while one is running. `isPending` only changes on the next render, so a guard that
 * reads it lets a double-click submit twice. A call with no answer after `STALL_MS` is `stalled`: `mutate` works again,
 * so a dialog that holds while `holding` lets the user retry or leave instead of being stuck.
 */
export function useExclusiveMutation<TData = unknown, TError = DefaultError, TVariables = void, TContext = unknown>(
  options: UseMutationOptions<TData, TError, TVariables, TContext>,
) {
  const mutation = useMutation(options);
  const call = useRef(0);
  const running = useRef(false);
  const [stalled, setStalled] = useState(false);
  const mutate = (variables: TVariables) => {
    if (running.current) return;
    running.current = true;
    setStalled(false);
    const id = ++call.current;
    const timer = setTimeout(() => {
      if (call.current !== id) return;
      running.current = false;
      setStalled(true);
    }, STALL_MS);
    mutation.mutate(variables, {
      onSettled: () => {
        clearTimeout(timer);
        if (call.current !== id) return;
        running.current = false;
        setStalled(false);
      },
    });
  };
  const reset = () => {
    call.current += 1;
    running.current = false;
    setStalled(false);
    mutation.reset();
  };
  return { ...mutation, mutate, reset, stalled, holding: mutation.isPending && !stalled };
}
