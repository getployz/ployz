import { useState } from "react";
import { toast } from "sonner";
import { Trash2Icon } from "lucide-react";
import { DeletionDialog, type DeletionItem } from "#/components/deletion-dialog";
import { Button } from "#/components/ui/button";
import { Item, ItemActions, ItemContent, ItemDescription, ItemTitle } from "#/components/ui/item";
import { plural } from "#/lib/plural";
import { loadNamespaceDataLossServerFn, removeNamespaceServerFn } from "#/modules/machines/namespace-cleanup.functions";
import { useStrayNamespaces } from "#/modules/machines/namespace-cleanup.queries";
import type { Server } from "#/modules/machines/use-servers";
import type { DataLossIdentity } from "#/modules/runtime/data-loss-identity";

/**
 * Namespaces running on this Server that no Environment owns, left behind by a failed teardown or a reset Store: they
 * hold hostnames and space, and nothing else can remove them. Each can be removed, with its Volumes.
 */
export function StrayNamespaces({ organizationSlug, server, servers }: { organizationSlug: string; server: Server; servers: readonly Server[] }) {
  const namespaces = server.services.flatMap((service) => service.namespace ?? []);
  const strays = useStrayNamespaces(organizationSlug, namespaces);
  const [removing, setRemoving] = useState<string | null>(null);
  if (strays.size === 0) return null;
  const servicesOf = (namespace: string) => server.services.filter((service) => service.namespace === namespace);
  const volumes = (lost: readonly DataLossIdentity[]) => lost.map((identity): DeletionItem => ({
    kind: "volume", name: identity.id.name,
    detail: servers.find((other) => other.machine.id === identity.id.machine_id)?.name,
  }));
  const services = (namespace: string) => servicesOf(namespace).map((service): DeletionItem => ({ kind: "service", name: service.name }));

  return (
    <>
      {[...strays].map((namespace) => (
        <Item key={namespace} variant="outline">
          <ItemContent>
            <ItemTitle>{namespace}</ItemTitle>
            <ItemDescription>Not in any Project · {plural(servicesOf(namespace).length, "service")}</ItemDescription>
          </ItemContent>
          <ItemActions>
            <Button variant="outline" size="sm" onClick={() => setRemoving(namespace)}>
              <Trash2Icon data-icon="inline-start" />
              Remove
            </Button>
          </ItemActions>
        </Item>
      ))}
      <DeletionDialog
        open={removing !== null}
        onOpenChange={(open) => { if (!open) setRemoving(null); }}
        title={`Remove ${removing ?? ""}?`}
        place={removing ?? ""}
        confirmLabel="Remove"
        sentence={<>No Project owns <span className="font-medium">{removing}</span>. Removing it from every server deletes:</>}
        items={removing === null ? [] : services(removing)}
        callbacks={{
          load: async () => {
            const namespace = removing ?? "";
            const lost = await loadNamespaceDataLossServerFn({ data: { organizationSlug, namespace } });
            return { items: [...services(namespace), ...volumes(lost)], evidence: lost };
          },
          confirm: async (lost) => {
            const namespace = removing ?? "";
            const outcome = await removeNamespaceServerFn({ data: { organizationSlug, namespace, confirmDataLoss: lost } });
            if ("missing" in outcome) {
              // The Servers hold more by now: the dialog lists it, to be typed for again.
              const all = [...lost, ...outcome.missing];
              return { items: [...services(namespace), ...volumes(all)], evidence: all };
            }
            if ("failed" in outcome) throw new Error(outcome.failed);
            toast(`${namespace} removed`);
          },
        }}
      />
    </>
  );
}
