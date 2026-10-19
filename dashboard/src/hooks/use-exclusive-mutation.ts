import { type DefaultError, useMutation, type UseMutationOptions } from "@tanstack/react-query";
import { useRef } from "react";

/**
 * A mutation that ignores `mutate` while one is running. `isPending` only changes on the next render, so a guard that
 * reads it lets a double-click submit twice.
 */
export function useExclusiveMutation<TData = unknown, TError = DefaultError, TVariables = void, TContext = unknown>(
  options: UseMutationOptions<TData, TError, TVariables, TContext>,
) {
  const mutation = useMutation(options);
  const running = useRef(false);
  const mutate = (variables: TVariables) => {
    if (running.current) return;
    running.current = true;
    mutation.mutate(variables, { onSettled: () => { running.current = false; } });
  };
  return { ...mutation, mutate };
}
