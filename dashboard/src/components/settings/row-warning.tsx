import type { ReactNode } from "react";
import { TriangleAlertIcon } from "lucide-react";
import { InfoHint } from "#/components/info-hint";

/**
 * A consequence on a row the user should weigh, in Attention Amber: it warns, it never blocks. One short line, the why
 * behind ⓘ, and the fix beside it. It sits under a row's hint.
 */
export function RowWarning({ why, action, children }: { why?: ReactNode; action?: ReactNode; children: ReactNode }) {
  return (
    <div role="note" className="flex items-start gap-1.5 text-sm text-warning">
      <TriangleAlertIcon aria-hidden className="size-4 shrink-0 translate-y-0.5" />
      <div className="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1">
        {/* The ⓘ flows with the last word, so it never wraps onto a line of its own. */}
        <span>{children}{why ? <> <span className="inline-flex align-middle"><InfoHint>{why}</InfoHint></span></> : null}</span>
        {action}
      </div>
    </div>
  );
}

/** Why several writers on one volume are a warning: said once, wherever it's warned. */
export const SHARED_VOLUME_WHY = "These containers write to the same files at the same time. Databases like Postgres and MySQL expect to be the only writer, so their data can be corrupted. Run 1 replica, or give each service its own volume.";
