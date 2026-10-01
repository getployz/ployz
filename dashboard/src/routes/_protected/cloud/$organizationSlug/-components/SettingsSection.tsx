import type { ReactNode } from "react";
import { TriangleAlertIcon } from "lucide-react";
import { FieldGroup } from "#/components/ui/field";
import { InfoHint } from "#/components/info-hint";

/**
 * A consequence on a row the user should weigh, in Attention Amber: it warns, it never blocks. One short line, the why
 * behind ⓘ, and the fix beside it.
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

/**
 * One section of settings, the same on every settings surface, panel or page: flat, a hairline above all but the first,
 * a title, and a line of description only where the title isn't enough. Its rows are Fields in one FieldGroup, so they
 * sit label left, control right where there's room, and stack where it's narrow. Danger rows are `DangerRow`s.
 */
export function SettingsSection({ id, title, description, action, children }: {
  id: string;
  title: string;
  description?: ReactNode;
  /** A control beside the title that acts on the whole section, like New environment. */
  action?: ReactNode;
  children: ReactNode;
}) {
  return (
    <section data-sec={id} aria-labelledby={`settings-${id}`} className="flex flex-col gap-4 border-t pt-6 first:border-t-0 first:pt-0">
      {/* The title never breaks: where the actions don't fit beside it, they wrap under it. */}
      <div className="flex flex-wrap items-start justify-between gap-x-4 gap-y-2">
        <div className="flex min-w-48 flex-1 flex-col gap-1">
          <h2 id={`settings-${id}`} className="text-base font-medium">{title}</h2>
          {description ? <p className="text-sm text-muted-foreground">{description}</p> : null}
        </div>
        {action ? <div className="flex shrink-0 flex-wrap justify-end gap-2">{action}</div> : null}
      </div>
      <FieldGroup>{children}</FieldGroup>
    </section>
  );
}
