import { DeletionDialog, type DeletionItem } from "#/components/deletion-dialog";
import type { PreparedDestructiveReview } from "#/components/destructive-volume/destructive-volume-review";
import { useEnvironmentPlace } from "#/routes/_protected/cloud/$organizationSlug/-components/deletion-items";

type Outcome = { state: "submitted" } | { state: "review_updated_evidence"; preparation: PreparedDestructiveReview };

/**
 * A save or deploy that deletes outside a Branch it doesn't keep: every service and volume that goes, with the size its
 * server reports, and the user types where.
 */
export function DestructiveChangesDialog({ open, onOpenChange, organizationSlug, environmentId, action, services, volumes, prepare, confirm }: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  organizationSlug: string;
  environmentId: string;
  action: "save" | "deploy";
  services: readonly DeletionItem[];
  /** The deleted volumes' names, shown until their servers answer. */
  volumes: readonly string[];
  prepare: () => Promise<PreparedDestructiveReview>;
  confirm: (preparation: PreparedDestructiveReview) => Promise<Outcome>;
}) {
  const place = useEnvironmentPlace(organizationSlug, environmentId);
  const itemsOf = (prepared: PreparedDestructiveReview): DeletionItem[] => [
    ...services,
    ...prepared.volumes.map(({ evidence }): DeletionItem => ({
      kind: "volume",
      name: evidence.volumeName,
      bytes: evidence.availability.status === "available" ? evidence.availability.usedBytes : undefined,
    })),
  ];

  return (
    <DeletionDialog
      open={open}
      onOpenChange={onOpenChange}
      title="Destructive changes"
      place={place}
      confirmLabel={action === "deploy" ? "Deploy" : "Save"}
      items={[...services, ...volumes.map((name): DeletionItem => ({ kind: "volume", name }))]}
      callbacks={{
        load: async () => {
          const prepared = await prepare();
          return { items: itemsOf(prepared), evidence: prepared };
        },
        confirm: async (prepared) => {
          const outcome = await confirm(prepared);
          if (outcome.state === "submitted") return;
          return { items: itemsOf(outcome.preparation), evidence: outcome.preparation };
        },
      }}
    />
  );
}
