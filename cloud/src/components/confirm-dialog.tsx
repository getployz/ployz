"use client";

import { useState, type ReactNode } from "react";
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
import { Spinner } from "#/components/ui/spinner";

export function ConfirmDialog({
  open,
  onOpenChange,
  title,
  description,
  actionLabel = "Confirm",
  cancelLabel = "Cancel",
  pendingLabel,
  variant = "default",
  onConfirm,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  title: ReactNode;
  description?: ReactNode;
  actionLabel?: string;
  cancelLabel?: string;
  pendingLabel?: string;
  variant?: "default" | "destructive";
  onConfirm: () => void | Promise<void>;
}) {
  const [pending, setPending] = useState(false);

  async function handleConfirm() {
    if (pending) return;
    setPending(true);
    try {
      await onConfirm();
    } finally {
      setPending(false);
    }
  }

  return (
    <AlertDialog open={open} onOpenChange={onOpenChange}>
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>{title}</AlertDialogTitle>
          {description ? (
            <AlertDialogDescription>{description}</AlertDialogDescription>
          ) : null}
        </AlertDialogHeader>
        <AlertDialogFooter>
          <AlertDialogCancel disabled={pending}>{cancelLabel}</AlertDialogCancel>
          <AlertDialogAction
            variant={variant === "destructive" ? "destructive" : "default"}
            disabled={pending}
            onClick={() => void handleConfirm()}
          >
            {pending ? <Spinner /> : null}
            {pending && pendingLabel ? pendingLabel : actionLabel}
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
}
