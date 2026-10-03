import { useState, type ReactNode } from "react";
import { useMutation } from "@tanstack/react-query";
import { PlusIcon } from "lucide-react";
import { toast } from "sonner";
import { Button } from "#/components/ui/button";
import type { ButtonVariants } from "#/components/ui/button-variants";
import { Alert, AlertDescription } from "#/components/ui/alert";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "#/components/ui/dialog";
import { Spinner } from "#/components/ui/spinner";
import { Switch } from "#/components/ui/switch";
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
          <DialogHeader>
            <DialogTitle>Add a server</DialogTitle>
            <DialogDescription>
              Expires{" "}
              {mintMutation.data
                ? expiryFormatter.format(new Date(mintMutation.data.expiresAt))
                : "in 24 hours"}
              .
            </DialogDescription>
          </DialogHeader>
          <ol className="flex flex-col gap-4">
            <Step number={1} title="Get a Linux server">
              <p className="text-muted-foreground">
                {withoutZfs
                  ? "Any systemd Linux, amd64 or arm64."
                  : "Ubuntu LTS, Debian 12–13 or Amazon Linux 2023 on a VM or bare metal, amd64 or arm64. Any provider works."}
              </p>
            </Step>
            <Step number={2} title="Run this as root">
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
              {withoutZfs ? null : (
                <p className="text-sm text-muted-foreground">
                  Sets up managed volumes (ZFS): enforced size limits now, with
                  backups, Server moves and zero-downtime migrations built on it
                  as they ship.
                </p>
              )}
            </Step>
          </ol>
          <div className="border-t pt-3 text-sm text-muted-foreground">
            {withoutZfs ? (
              <div className="flex items-start gap-2.5">
                <Switch
                  id="without-zfs"
                  className="mt-0.5"
                  checked
                  onCheckedChange={setWithoutZfs}
                />
                <div>
                  <label htmlFor="without-zfs" className="text-foreground">
                    Without ZFS{" "}
                    <span className="text-muted-foreground">
                      · not recommended
                    </span>
                  </label>
                  <p>
                    Volumes get no size limits, and this Server misses backups,
                    Server moves and zero-downtime migrations as they arrive.{" "}
                    <InlineLink onClick={() => setWithoutZfs(false)}>
                      Use ZFS instead
                    </InlineLink>
                  </p>
                </div>
              </div>
            ) : (
              <p>
                Other Linux, or can’t run ZFS?{" "}
                <InlineLink onClick={() => setWithoutZfs(true)}>
                  Start without it
                </InlineLink>
              </p>
            )}
          </div>
        </DialogContent>
      </Dialog>
    </>
  );
}

function Step({ number, title, children }: { number: number; title: string; children: ReactNode }) {
  return (
    <li className="flex gap-3">
      <span className="flex size-5.5 shrink-0 items-center justify-center rounded-full border text-xs text-muted-foreground">
        {number}
      </span>
      <div className="flex min-w-0 flex-1 flex-col gap-2">
        <p>{title}</p>
        {children}
      </div>
    </li>
  );
}

function InlineLink({ onClick, children }: { onClick: () => void; children: ReactNode }) {
  return (
    <Button type="button" variant="link" className="h-auto p-0 text-foreground underline" onClick={onClick}>
      {children}
    </Button>
  );
}
