"use client";

import { useId, useState, type ReactNode } from "react";
import { AlertTriangleIcon } from "lucide-react";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogMedia,
  AlertDialogTitle,
} from "#/components/ui/alert-dialog";
import { Input } from "#/components/ui/input";
import { Label } from "#/components/ui/label";
import { Spinner } from "#/components/ui/spinner";

export type ConfirmDestructiveResource = {
  id: string;
  icon?: ReactNode;
  label: ReactNode;
};

export function ConfirmDestructiveDialog({
  open,
  onOpenChange,
  title,
  description,
  resources,
  confirmPhrase,
  actionLabel = "Delete",
  pendingLabel,
  onConfirm,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  title: ReactNode;
  description?: ReactNode;
  resources?: ConfirmDestructiveResource[];
  confirmPhrase: string;
  actionLabel?: string;
  pendingLabel?: string;
  onConfirm: () => void | Promise<void>;
}) {
  return (
    <AlertDialog open={open} onOpenChange={onOpenChange}>
      {open ? (
        <ConfirmDestructiveDialogContent
          title={title}
          description={description}
          resources={resources}
          confirmPhrase={confirmPhrase}
          actionLabel={actionLabel}
          pendingLabel={pendingLabel}
          onConfirm={onConfirm}
        />
      ) : null}
    </AlertDialog>
  );
}

function ConfirmDestructiveDialogContent({
  title,
  description,
  resources,
  confirmPhrase,
  actionLabel,
  pendingLabel,
  onConfirm,
}: {
  title: ReactNode;
  description?: ReactNode;
  resources?: ConfirmDestructiveResource[];
  confirmPhrase: string;
  actionLabel: string;
  pendingLabel?: string;
  onConfirm: () => void | Promise<void>;
}) {
  const inputId = useId();
  const [value, setValue] = useState("");
  const [pending, setPending] = useState(false);

  const matches = value === confirmPhrase;

  async function handleConfirm() {
    if (!matches || pending) return;
    setPending(true);
    try {
      await onConfirm();
    } finally {
      setPending(false);
    }
  }

  return (
    <AlertDialogContent>
      <AlertDialogHeader>
        <AlertDialogMedia>
          <AlertTriangleIcon className="text-destructive" />
        </AlertDialogMedia>
        <AlertDialogTitle>{title}</AlertDialogTitle>
        {description ? (
          <AlertDialogDescription>{description}</AlertDialogDescription>
        ) : null}
      </AlertDialogHeader>

      {resources?.length ? (
        <ul className="flex flex-col gap-1.5 rounded-md border p-3 text-sm">
          {resources.map((resource) => (
            <li key={resource.id} className="flex items-center gap-2">
              {resource.icon}
              <span>{resource.label}</span>
            </li>
          ))}
        </ul>
      ) : null}

      <div className="flex flex-col gap-2">
        <Label htmlFor={inputId} className="font-normal">
          Type <strong className="font-mono">{confirmPhrase}</strong> to confirm
        </Label>
        <Input
          id={inputId}
          autoFocus
          value={value}
          onChange={(event) => setValue(event.target.value)}
          placeholder={confirmPhrase}
          disabled={pending}
          onKeyDown={(event) => {
            if (event.key === "Enter" && matches && !pending) {
              event.preventDefault();
              void handleConfirm();
            }
          }}
        />
      </div>

      <AlertDialogFooter>
        <AlertDialogCancel disabled={pending}>Cancel</AlertDialogCancel>
        <AlertDialogAction
          variant="destructive"
          disabled={!matches || pending}
          onClick={() => void handleConfirm()}
        >
          {pending ? <Spinner /> : null}
          {pending && pendingLabel ? pendingLabel : actionLabel}
        </AlertDialogAction>
      </AlertDialogFooter>
    </AlertDialogContent>
  );
}
