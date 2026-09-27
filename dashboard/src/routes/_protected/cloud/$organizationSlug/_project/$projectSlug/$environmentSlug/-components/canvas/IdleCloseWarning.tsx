import { ClockIcon } from "lucide-react";
import { Button } from "#/components/ui/button";
import { Item, ItemActions, ItemContent, ItemMedia, ItemTitle } from "#/components/ui/item";
import { cn } from "#/lib/utils";
import { useIdleClose, useKeepBranch } from "#/modules/branches/branch.collection";

/** From day 5 without a deploy, a Branch's canvas says when it closes itself, with Keep. */
export function IdleCloseWarning({ organizationSlug, environmentId, className }: {
  organizationSlug: string;
  environmentId: string;
  className?: string;
}) {
  const idle = useIdleClose(organizationSlug, environmentId);
  const keepBranch = useKeepBranch(organizationSlug);
  if (idle.kind !== "warn") return null;
  return (
    <Item role="status" variant="outline" size="xs" className={cn("pointer-events-auto w-fit bg-background", className)}>
      <ItemMedia variant="icon"><ClockIcon /></ItemMedia>
      <ItemContent><ItemTitle>Closes in {idle.daysLeft} {idle.daysLeft === 1 ? "day" : "days"}</ItemTitle></ItemContent>
      <ItemActions><Button variant="ghost" size="sm" onClick={() => keepBranch(environmentId, true)}>Keep it</Button></ItemActions>
    </Item>
  );
}
