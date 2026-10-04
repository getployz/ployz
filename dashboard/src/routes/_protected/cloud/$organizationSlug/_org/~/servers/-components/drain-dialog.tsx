import { BoxIcon } from "lucide-react";
import { ConfirmDialog } from "#/components/confirm-dialog";
import { AlertDialogDescription } from "#/components/ui/alert-dialog";
import { Item, ItemContent, ItemGroup, ItemMedia, ItemTitle } from "#/components/ui/item";
import { listNames } from "#/lib/plural";

/** The plain confirm: what runs here, what stays, and that nothing comes back. No dry run. */
export function DrainDialog({ serverName, names, unowned, open, onOpenChange, onConfirm }: {
  serverName: string;
  /** What runs here by name, Namespaces no Project owns left out (`drainDialogNames`). */
  names: readonly string[];
  /** What runs here in Namespaces no Project owns: the Drain leaves it. */
  unowned: readonly string[];
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onConfirm: () => void;
}) {
  const notes = [
    unowned.length === 0
      ? null
      : `${listNames([...unowned])} ${unowned.length === 1 ? "stays" : "stay"}: no project owns ${unowned.length === 1 ? "it" : "them"}.`,
    "Services that use a volume here stay.",
  ].filter((sentence) => sentence !== null).join(" ");
  return (
    <ConfirmDialog
      open={open}
      onOpenChange={onOpenChange}
      title={`Drain ${serverName}?`}
      description={`Services turn off for ${serverName}, and what runs here moves to your other servers one at a time.`}
      actionLabel="Drain"
      onConfirm={onConfirm}
    >
      {names.length === 0 ? (
        <AlertDialogDescription>
          {unowned.length === 0 ? "Nothing runs here now." : "Nothing here for Drain to move."}
        </AlertDialogDescription>
      ) : (
        <ItemGroup aria-label={`Running on ${serverName}`}>
          {names.map((name) => (
            <Item key={name} variant="outline" size="xs">
              <ItemMedia variant="icon"><BoxIcon /></ItemMedia>
              <ItemContent><ItemTitle>{name}</ItemTitle></ItemContent>
            </Item>
          ))}
        </ItemGroup>
      )}
      <AlertDialogDescription>{notes}</AlertDialogDescription>
      <AlertDialogDescription>Turning services back on doesn’t move anything back.</AlertDialogDescription>
    </ConfirmDialog>
  );
}
