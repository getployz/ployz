import type { RuntimeWatchView } from "@ployz/sdk";

/** The Servers, by id, that hold the Cluster's Sealing Key and those that lack it, or null while either is empty. */
function sealingGap(frame: Pick<RuntimeWatchView, "machines">): string | null {
  const holding: string[] = [];
  const lacking: string[] = [];
  for (const { machine } of frame.machines) {
    const kind = machine.runtime.sealing?.kind;
    if (kind === "held") holding.push(machine.id);
    else if (kind === "needs_access" || kind === "awaits_recovery") lacking.push(machine.id);
  }
  if (holding.length === 0 || lacking.length === 0) return null;
  return `${holding.sort().join(",")}|${lacking.sort().join(",")}`;
}

/**
 * Passes the frames through, and calls `gap` when a frame shows a Server holding the Sealing Key while another lacks
 * it, once per change of those Servers. A recover on one Server is how such a gap opens after enrollment.
 */
export async function* onSealingGap<Frame extends Pick<RuntimeWatchView, "machines">>(
  frames: AsyncIterable<Frame>,
  gap: () => void,
): AsyncGenerator<Frame> {
  let before: string | null = null;
  for await (const frame of frames) {
    const now = sealingGap(frame);
    if (now !== null && now !== before) gap();
    before = now;
    yield frame;
  }
}
