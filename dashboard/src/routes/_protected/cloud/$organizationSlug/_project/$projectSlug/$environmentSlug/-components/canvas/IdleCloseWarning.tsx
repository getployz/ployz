import { ClockIcon } from "lucide-react";
import { Button } from "#/components/ui/button";
import { useIdleClose, useKeepBranch } from "#/modules/branches/branch.collection";

/** From day 5 without a deploy, a Branch's canvas says when it closes itself, with Keep. */
export function IdleCloseWarning({ organizationSlug, environmentId }: { organizationSlug: string; environmentId: string }) {
  const idle = useIdleClose(organizationSlug, environmentId);
  const keepBranch = useKeepBranch(organizationSlug);
  if (idle.kind !== "warn") return null;
  return (
    <div role="status" className="pointer-events-auto flex items-center gap-2 rounded-lg border bg-background pl-2.5 text-sm shadow-xs">
      <ClockIcon className="size-4 text-muted-foreground" />
      Closes in {idle.daysLeft} {idle.daysLeft === 1 ? "day" : "days"}
      <Button variant="ghost" size="sm" onClick={() => keepBranch(environmentId, true)}>Keep it</Button>
    </div>
  );
}
