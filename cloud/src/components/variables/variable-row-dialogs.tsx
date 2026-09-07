import { ConfirmDialog } from "#/components/confirm-dialog";
import { ConfirmDestructiveDialog } from "#/components/confirm-destructive-dialog";

export function VariableRowDialogs({
  variableKey,
  confirmSealOpen,
  confirmDeleteOpen,
  onConfirmDelete,
  onConfirmSeal,
  onDeleteOpenChange,
  onSealOpenChange,
}: {
  variableKey: string;
  confirmSealOpen: boolean;
  confirmDeleteOpen: boolean;
  onConfirmDelete: () => Promise<void>;
  onConfirmSeal: () => Promise<void>;
  onDeleteOpenChange: (open: boolean) => void;
  onSealOpenChange: (open: boolean) => void;
}) {
  return (
    <>
      <ConfirmDialog
        open={confirmSealOpen}
        onOpenChange={onSealOpenChange}
        title="Seal Variable"
        description={
          <>
            Sealing <strong>{variableKey}</strong> makes its value unavailable
            for reveal, copy, and edit in this UI. This change cannot be undone
            here.
          </>
        }
        actionLabel="Seal variable"
        pendingLabel="Sealing…"
        onConfirm={onConfirmSeal}
      />

      <ConfirmDestructiveDialog
        open={confirmDeleteOpen}
        onOpenChange={onDeleteOpenChange}
        title="Delete Variable"
        description={
          <>
            You are <span className="text-destructive">deleting</span> the
            variable <strong>{variableKey}</strong>.
          </>
        }
        confirmPhrase={variableKey}
        actionLabel="Delete"
        onConfirm={onConfirmDelete}
      />
    </>
  );
}
