import type { ServiceSettingChange } from "@ployz/sdk";
import { plural } from "#/lib/plural";
import { settingText } from "#/modules/config-store/store-services";
import { useRuntimeLens } from "#/modules/runtime/use-runtime-lens";
import { useRuntimeService } from "#/providers/runtime-provider";
import { runtimeLine } from "../../../-components/canvas/node-status";
import { StatusLine } from "../../../-components/canvas/node-status-view";
import type { StoreService } from "./StoreServiceDrawer";

/** Its deployed value while the next Deploy changes it: what runs now, never what's staged. */
const deployed = (changes: Map<string, ServiceSettingChange>, name: string, current: string) => {
  const change = changes.get(name);
  return change ? settingText(change.before) : current;
};

/**
 * One line under a service's name: what runs now and where, from the same evidence as its canvas card, then what it is
 * (source, replicas, the address other services use). The card's word for its state, never a guess.
 */
export function ServiceSummary({ state, namespace }: { state: StoreService; namespace: string | null }) {
  const { organizationSlug, service, rows, changes } = state;
  const lens = useRuntimeLens(organizationSlug);
  const privateDns = deployed(changes, "privateDns", settingText(rows.get("privateDns")?.value) || service.private_dns);
  const { runtime } = useRuntimeService(namespace ? `${namespace}/${privateDns}` : "");
  const replicas = Number(deployed(changes, "replicas", settingText(rows.get("replicas")?.value ?? rows.get("replicas")?.default)));
  const status = runtimeLine(service, runtime, { lens, desiredReplicas: Number.isInteger(replicas) ? replicas : null, deploying: false });
  const machineIds = new Set(runtime?.containers.map((container) => container.machineId));
  const servers = lens.machines.filter((machine) => machineIds.has(machine.id)).map((machine) => machine.name);
  const source = settingText(rows.get("repository")?.value) || settingText(rows.get("image")?.value);
  const port = settingText(rows.get("env.PORT")?.value);

  const details = [
    servers.length ? `on ${servers.join(", ")}` : null,
    source || null,
    Number.isInteger(replicas) ? plural(replicas, "replica") : null,
    `${privateDns}.internal${port ? `:${port}` : ""}`,
  ].filter(Boolean).join(" · ");

  // One line on every width: the rest truncates, and the title holds it whole.
  return (
    <div className="flex min-w-0 items-center gap-2 text-sm text-muted-foreground" title={`${status.word} · ${details}`}>
      <StatusLine status={status} issues={null} className="shrink-0" />
      <span className="min-w-0 truncate">· {details}</span>
    </div>
  );
}
