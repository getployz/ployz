import { ConfirmDialog } from "#/components/confirm-dialog";

/** Sealing can't be undone here, so it asks; deleting is staged, so it doesn't. */
export function VariableRowDialogs({
  variableKey,
  confirmSealOpen,
  onConfirmSeal,
  onSealOpenChange,
}: {
  variableKey: string;
  confirmSealOpen: boolean;
  onConfirmSeal: () => void;
  onSealOpenChange: (open: boolean) => void;
}) {
  return (
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
  );
}
