import { useState } from "react";
import { useMutation } from "@tanstack/react-query";
import { ChevronRightIcon, PlusIcon } from "lucide-react";
import { toast } from "sonner";
import { Button } from "#/components/ui/button";
import type { ButtonVariants } from "#/components/ui/button-variants";
import { Checkbox } from "#/components/ui/checkbox";
import { Alert, AlertDescription } from "#/components/ui/alert";
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "#/components/ui/collapsible";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "#/components/ui/dialog";
import { Spinner } from "#/components/ui/spinner";
import {
  Field,
  FieldContent,
  FieldDescription,
  FieldLabel,
} from "#/components/ui/field";
import { mintMachineEnrollmentServerFn } from "#/modules/machines/enrollment.functions";
import { CopyBlock } from "./copy-block";

const expiryFormatter = new Intl.DateTimeFormat("en-US", {
  year: "numeric",
  month: "short",
  day: "numeric",
  hour: "numeric",
  minute: "2-digit",
  timeZone: "UTC",
  timeZoneName: "short",
});

export function AddServerDialog({
  organizationSlug,
  label = "Add server",
  variant = "ink",
}: {
  organizationSlug: string;
  label?: string;
  variant?: ButtonVariants["variant"];
}) {
  const [open, setOpen] = useState(false);
  const [withoutZfs, setWithoutZfs] = useState(false);
  const mintMutation = useMutation({
    mutationFn: () =>
      mintMachineEnrollmentServerFn({ data: { organizationSlug } }),
    onError: (error) => {
      toast.error(
        error instanceof Error
          ? error.message
          : "The server command couldn’t be created. Close this dialog and try again.",
      );
    },
  });

  function handleOpenChange(nextOpen: boolean) {
    setOpen(nextOpen);
    if (!nextOpen) {
      setWithoutZfs(false);
      mintMutation.reset();
    }
  }

  return (
    <>
      <Button
        type="button"
        variant={variant}
        onClick={() => {
          setOpen(true);
          mintMutation.mutate();
        }}
      >
        <PlusIcon data-icon="inline-start" />
        {label}
      </Button>
      <Dialog open={open} onOpenChange={handleOpenChange}>
        <DialogContent className="sm:max-w-xl">
          <div className="flex flex-col gap-3">
            <DialogHeader>
              <DialogTitle>Add a server</DialogTitle>
              <DialogDescription>
                Run once as administrator · Expires{" "}
                {mintMutation.data
                  ? expiryFormatter.format(
                      new Date(mintMutation.data.expiresAt),
                    )
                  : "in 24 hours"}
                .
              </DialogDescription>
            </DialogHeader>
            <p className="text-sm text-muted-foreground">
              Managed volumes on. Enforced size limits today. Backups, Server
              moves and zero-downtime migrations build on this as they ship.
            </p>
            {mintMutation.isPending ? (
              <div className="flex items-center gap-2 text-muted-foreground">
                <Spinner />
                Creating command…
              </div>
            ) : mintMutation.data ? (
              <CopyBlock
                value={`${mintMutation.data.command}${withoutZfs ? " --storage none" : ""}`}
              />
            ) : (
              <Alert variant="destructive">
                <AlertDescription>
                  Command unavailable. Close and try again.
                </AlertDescription>
              </Alert>
            )}
            <Collapsible className="flex flex-col gap-3">
              <CollapsibleTrigger
                render={
                  <Button
                    type="button"
                    variant="ghost"
                    size="sm"
                    className="group self-start"
                  />
                }
              >
                Advanced
                <ChevronRightIcon
                  data-icon="inline-end"
                  className="transition-transform group-data-[panel-open]:rotate-90"
                />
              </CollapsibleTrigger>
              <CollapsibleContent render={<Field orientation="horizontal" />}>
                <Checkbox
                  id="without-zfs"
                  checked={withoutZfs}
                  onCheckedChange={setWithoutZfs}
                />
                <FieldContent>
                  <FieldLabel htmlFor="without-zfs">
                    Start without ZFS (not recommended)
                  </FieldLabel>
                  {withoutZfs ? (
                    <FieldDescription>
                      Volumes on this Server get no size limits, and it won’t
                      get backups, Server moves or zero-downtime migrations as
                      they arrive. Only for hosts that can’t run ZFS.
                    </FieldDescription>
                  ) : null}
                </FieldContent>
              </CollapsibleContent>
            </Collapsible>
          </div>
        </DialogContent>
      </Dialog>
    </>
  );
}
