const PATIENCE_HOURS = 6;
const PATIENCE_SECONDS = PATIENCE_HOURS * 60 * 60;
export const PATIENCE = `${PATIENCE_HOURS}h`;
const FIRST_WAIT_SECONDS = 5;
const LONGEST_WAIT_SECONDS = 300;

/** A list, not a clock, so a replayed run sleeps under the same step ids. */
export const PATIENCE_WAITS: readonly number[] = (() => {
  const waits: number[] = [];
  let waited = 0;
  for (let wait = FIRST_WAIT_SECONDS; waited < PATIENCE_SECONDS; wait = Math.min(wait * 2, LONGEST_WAIT_SECONDS)) {
    const next = Math.min(wait, PATIENCE_SECONDS - waited);
    waits.push(next);
    waited += next;
  }
  return waits;
})();
