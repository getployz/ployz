import { BoxIcon } from "lucide-react";
import { ConfirmDialog } from "#/components/confirm-dialog";
import { Item, ItemContent, ItemGroup, ItemMedia, ItemTitle } from "#/components/ui/item";

/** The plain confirm: what runs here, what stays, and that nothing comes back. No dry run. */
export function DrainDialog({ serverName, names, open, onOpenChange, onConfirm }: {
  serverName: string;
  /** What runs here by name, Namespaces no Project owns left out (`drainDialogNames`). */
  names: readonly string[];
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onConfirm: () => void;
}) {
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
        <p>Nothing runs here now.</p>
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
      <p>Services that use a volume here stay.</p>
      <p className="text-muted-foreground">Turning services back on doesn’t move anything back.</p>
    </ConfirmDialog>
  );
}
