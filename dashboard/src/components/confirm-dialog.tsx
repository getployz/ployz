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
  children,
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
  /** A body between the header and the footer, for what a description's one paragraph can't hold, like a list. */
  children?: ReactNode;
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
        {children ? <div className="flex flex-col gap-3 text-sm">{children}</div> : null}
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
