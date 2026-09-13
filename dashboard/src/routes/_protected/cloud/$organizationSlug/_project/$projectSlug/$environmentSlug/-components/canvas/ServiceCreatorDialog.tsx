import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "#/components/ui/dialog";
import { ServiceCreateCommand } from "#/components/service-create-command";
import type { createServiceServerFn } from "#/modules/environment-design/service-functions";
import type { CreatorPanel, FlowPosition } from "./types";

export function ServiceCreatorDialog({
  open,
  onOpenChange,
  panel,
  position,
  params,
  onCreateVariableGroup,
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
  onCreateVariableGroup: () => void;
  onCreateVolume: () => void;
  onCreated: (
    result: Awaited<ReturnType<typeof createServiceServerFn>>["data"],
  ) => void | Promise<void>;
}) {
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
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
          onCreateVariableGroup={onCreateVariableGroup}
          onCreateVolume={onCreateVolume}
          onCreated={onCreated}
        />
      </DialogContent>
    </Dialog>
  );
}
