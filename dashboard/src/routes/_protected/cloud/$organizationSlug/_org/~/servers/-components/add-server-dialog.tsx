import { useState } from "react";
import { useMutation } from "@tanstack/react-query";
import { PlusIcon } from "lucide-react";
import { toast } from "sonner";
import { Button } from "#/components/ui/button";
import { Checkbox } from "#/components/ui/checkbox";
import { Alert, AlertDescription } from "#/components/ui/alert";
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
}: {
  organizationSlug: string;
}) {
  const [open, setOpen] = useState(false);
  const [managedVolumes, setManagedVolumes] = useState(false);
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
      setManagedVolumes(false);
      mintMutation.reset();
    }
  }

  return (
    <>
      <Button
        type="button"
        variant="outline"
        size="lg"
        onClick={() => {
          setOpen(true);
          mintMutation.mutate();
        }}
      >
        <PlusIcon data-icon="inline-start" />
        Add machine
      </Button>
      <Dialog open={open} onOpenChange={handleOpenChange}>
        <DialogContent className="sm:max-w-xl">
          <div className="flex flex-col gap-3">
            <DialogHeader className="gap-1">
              <DialogTitle>Add machine</DialogTitle>
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
            <Field orientation="horizontal">
              <Checkbox
                id="managed-volumes"
                checked={managedVolumes}
                onCheckedChange={setManagedVolumes}
              />
              <FieldContent>
                <FieldLabel htmlFor="managed-volumes">
                  Managed volumes
                </FieldLabel>
                <FieldDescription>
                  Unlock zero-downtime server migrations, efficient backups,
                  and instant rollbacks. You can opt in later.
                </FieldDescription>
              </FieldContent>
            </Field>
            {mintMutation.isPending ? (
              <div className="flex items-center gap-2 text-muted-foreground">
                <Spinner />
                Creating command…
              </div>
            ) : mintMutation.data ? (
              <CopyBlock
                value={`${mintMutation.data.command}${managedVolumes ? " --storage zfs" : ""}`}
              />
            ) : (
              <Alert variant="destructive">
                <AlertDescription>
                  Command unavailable. Close and try again.
                </AlertDescription>
              </Alert>
            )}
          </div>
        </DialogContent>
      </Dialog>
    </>
  );
}
