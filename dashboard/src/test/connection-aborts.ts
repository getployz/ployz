/**
 * Whether each connection a fake Ployz connect got has been aborted. It listens on each signal as the SDK's connect
 * does: Node works out an unlistened `AbortSignal.any` only when it is read, from sources it holds weakly, so a read
 * after its aborted controller was collected says it never aborted.
 */
export function connectionAborts() {
  const aborted: boolean[] = [];
  return {
    watch: (signal: AbortSignal | undefined) => {
      if (signal === undefined) return;
      const at = aborted.push(signal.aborted) - 1;
      signal.addEventListener("abort", () => { aborted[at] = true; }, { once: true });
    },
    aborted: () => [...aborted],
  };
}

export type ConnectionAborts = ReturnType<typeof connectionAborts>;
