import { useState } from "react";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "#/components/ui/dialog";
import { ServiceCreateCommand } from "#/components/service-create-command";
import type { CreatorPanel, FlowPosition } from "./types";

export function ServiceCreatorDialog({
  open,
  onOpenChange,
  panel,
  position,
  params,
  onCreateVolume,
  onCreated,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  panel: CreatorPanel;
  position: FlowPosition;
  params: {
    organizationSlug: string;
    projectSlug: string;
    environmentSlug: string;
  };
  onCreateVolume: () => void;
  onCreated: (result: { service: { id: string } }, stillHere: boolean) => void | Promise<void>;
}) {
  // Escape or a click outside can't close it mid-create: the new service opens once it's saved.
  const [creating, setCreating] = useState(false);
  return (
    <Dialog open={open} onOpenChange={(next) => { if (next || !creating) onOpenChange(next); }}>
      <DialogContent
        className="max-w-md overflow-hidden"
        padding="none"
        showCloseButton={false}
        surface="unstyled"
      >
        <DialogHeader className="sr-only">
          <DialogTitle>Create service</DialogTitle>
          <DialogDescription>
            Choose a source for the new service.
          </DialogDescription>
        </DialogHeader>
        <ServiceCreateCommand
          mode="service"
          initialPanel={panel}
          organizationSlug={params.organizationSlug}
          projectSlug={params.projectSlug}
          environmentSlug={params.environmentSlug}
          canvasPosition={position}
          onCreateVolume={onCreateVolume}
          onCreated={onCreated}
          onPendingChange={setCreating}
        />
      </DialogContent>
    </Dialog>
  );
}
