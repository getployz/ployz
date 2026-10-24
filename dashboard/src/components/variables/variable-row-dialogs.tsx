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
          Seal <strong>{variableKey}</strong>? You can't reveal or edit its value here afterwards.
        </>
      }
      actionLabel="Seal variable"
      pendingLabel="Sealing…"
      variant="destructive"
      onConfirm={onConfirmSeal}
    />
  );
}
