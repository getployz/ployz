"use client";

import { useState, useTransition } from "react";
import { RotateCcwIcon } from "lucide-react";
import { toast } from "sonner";
import {
  Alert,
  AlertAction,
  AlertDescription,
  AlertTitle,
} from "#/components/ui/alert";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "#/components/ui/alert-dialog";
import { Button } from "#/components/ui/button";
import { Checkbox } from "#/components/ui/checkbox";
import {
  Field,
  FieldContent,
  FieldGroup,
  FieldLabel,
} from "#/components/ui/field";
import { Spinner } from "#/components/ui/spinner";
import type { OrganizationEnrollmentStatus } from "#/modules/machines/enrollment";

export function PendingEnrollmentResetSection({
  status,
  onReset,
  onCompleted,
}: {
  status: OrganizationEnrollmentStatus;
  onReset: (input: { confirmedFounderStoppedOrErased: true }) => Promise<void>;
  onCompleted?: () => void;
}) {
  const [open, setOpen] = useState(false);
  const [confirmed, setConfirmed] = useState(false);
  const [resetting, startReset] = useTransition();

  function handleOpenChange(nextOpen: boolean) {
    if (resetting) return;
    setOpen(nextOpen);
    if (!nextOpen) setConfirmed(false);
  }

  function handleReset() {
    if (!confirmed || resetting) return;
    startReset(async () => {
      try {
        await onReset({ confirmedFounderStoppedOrErased: true });
        setOpen(false);
        setConfirmed(false);
        toast.success("Founding attempt reset.");
        onCompleted?.();
      } catch (error) {
        toast.error(
          error instanceof Error
            ? error.message
            : "The founding attempt couldn’t be reset.",
        );
      }
    });
  }

  return (
    <>
      <Alert variant={status === "pending" ? "destructive" : "default"}>
        <AlertTitle>
          {status === "unclaimed"
            ? "Organization enrollment unclaimed"
            : status === "pending"
              ? "Founding attempt pending"
              : "Organization enrollment ready"}
        </AlertTitle>
        <AlertDescription>
          {status === "unclaimed"
            ? "No Server has claimed founding for this Organization."
            : status === "pending"
              ? "Another Server cannot found this Organization until the current attempt finishes or is safely reset."
              : "This Organization has an enrolled Cluster."}
        </AlertDescription>
        {status === "pending" ? (
          <AlertAction>
            <Button variant="destructive" onClick={() => setOpen(true)}>
              <RotateCcwIcon data-icon="inline-start" />
              Reset founding attempt
            </Button>
          </AlertAction>
        ) : null}
      </Alert>

      {status === "pending" ? (
        <AlertDialog open={open} onOpenChange={handleOpenChange}>
          <AlertDialogContent>
            <AlertDialogHeader>
              <AlertDialogTitle>Reset pending enrollment?</AlertDialogTitle>
              <AlertDialogDescription>
                This only resets Cloud enrollment. It does not erase or destroy
                the old Machine. Stop or erase it first; if it returns, it may
                remain an orphaned Cluster.
              </AlertDialogDescription>
            </AlertDialogHeader>
            <FieldGroup>
              <Field orientation="horizontal" data-disabled={resetting}>
                <Checkbox
                  id="founder-stopped-or-erased"
                  checked={confirmed}
                  onCheckedChange={(checked) => setConfirmed(checked === true)}
                  disabled={resetting}
                />
                <FieldContent>
                  <FieldLabel htmlFor="founder-stopped-or-erased">
                    I confirm the old Machine has been stopped or erased.
                  </FieldLabel>
                </FieldContent>
              </Field>
            </FieldGroup>
            <AlertDialogFooter>
              <AlertDialogCancel disabled={resetting}>Cancel</AlertDialogCancel>
              <AlertDialogAction
                variant="destructive"
                disabled={!confirmed || resetting}
                onClick={handleReset}
              >
                {resetting ? <Spinner data-icon="inline-start" /> : null}
                {resetting ? "Resetting…" : "Reset enrollment"}
              </AlertDialogAction>
            </AlertDialogFooter>
          </AlertDialogContent>
        </AlertDialog>
      ) : null}
    </>
  );
}
