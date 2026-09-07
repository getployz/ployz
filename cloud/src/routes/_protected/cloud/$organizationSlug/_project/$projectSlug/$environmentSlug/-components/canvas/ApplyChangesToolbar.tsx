import { MoreVerticalIcon, Trash2Icon } from "lucide-react";
import { Button } from "#/components/ui/button";
import { Badge } from "#/components/ui/badge";
import type { CanvasDeploymentEvidence } from "#/modules/environment-design/canvas-environment-change-state";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuGroup,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "#/components/ui/dropdown-menu";
import { Kbd } from "#/components/ui/kbd";
import { cn } from "#/lib/utils";

export function ApplyChangesToolbar({
  className,
  canDiscardAll,
  canSaveWithoutDeploying,
  canDeploy,
  deploymentEvidence,
  totalChanges,
  onDeploy,
  onDiscardAll,
  onOpenDetails,
  onSaveWithoutDeploying,
}: {
  className?: string;
  canDiscardAll: boolean;
  canSaveWithoutDeploying: boolean;
  canDeploy: boolean;
  deploymentEvidence?: CanvasDeploymentEvidence | null;
  totalChanges: number;
  onDeploy: () => void;
  onDiscardAll: () => void;
  onOpenDetails: () => void;
  onSaveWithoutDeploying: () => void;
}) {
  return (
    <div
      className={cn(
        "pointer-events-auto flex items-center gap-2 rounded-xl border bg-background/95 p-2 shadow-md backdrop-blur",
        className,
      )}
    >
      <span className="px-2 text-sm font-medium whitespace-nowrap">
        Apply {totalChanges} {totalChanges === 1 ? "change" : "changes"}
      </span>
      {deploymentEvidence ? (
        <Badge variant="secondary">Deployment {deploymentEvidence.status}</Badge>
      ) : null}
      <Button variant="outline" onClick={onOpenDetails}>
        Details
      </Button>
      {canDeploy ? <Button onClick={onDeploy}>
        Deploy
        <Kbd className="ml-1 hidden sm:inline-flex">⇧+Enter</Kbd>
      </Button> : null}
      <DropdownMenu>
        <DropdownMenuTrigger render={<Button variant="ghost" size="icon" />}>
          <MoreVerticalIcon />
          <span className="sr-only">More actions</span>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end" className="w-52 min-w-52">
          <DropdownMenuGroup>
            <DropdownMenuItem
              disabled={!canSaveWithoutDeploying}
              variant="default"
              onClick={onSaveWithoutDeploying}
            >
              Save without deploying
            </DropdownMenuItem>
            <DropdownMenuSeparator />
            <DropdownMenuItem
              disabled={!canDiscardAll}
              variant="destructive"
              onClick={onDiscardAll}
            >
              <Trash2Icon />
              Discard Changes
            </DropdownMenuItem>
          </DropdownMenuGroup>
        </DropdownMenuContent>
      </DropdownMenu>
    </div>
  );
}
