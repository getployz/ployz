import type { ServiceSettingChange } from "@ployz/sdk";
import { TriangleAlertIcon } from "lucide-react";
import { Item, ItemContent, ItemDescription, ItemMedia, ItemTitle } from "#/components/ui/item";
import { plural } from "#/lib/plural";
import { healthcheckOf, type Healthcheck } from "#/modules/config-store/healthcheck";
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

function useServiceStatus({ organizationSlug, service, rows, changes }: StoreService, namespace: string | null) {
  const lens = useRuntimeLens(organizationSlug);
  const privateDns = deployed(changes, "privateDns", settingText(rows.get("privateDns")?.value) || service.private_dns);
  const { runtime } = useRuntimeService(namespace ? `${namespace}/${privateDns}` : "");
  const replicas = Number(deployed(changes, "replicas", settingText(rows.get("replicas")?.value ?? rows.get("replicas")?.default)));
  const status = runtimeLine(service, runtime, { lens, desiredReplicas: Number.isInteger(replicas) ? replicas : null, deploying: false });
  return { lens, privateDns, runtime, replicas, status };
}

/**
 * One line under a service's name: what runs now and where, from the same evidence as its canvas card, then what it is
 * (source, replicas, the address other services use). The card's word for its state, never a guess.
 */
export function ServiceSummary({ state, namespace }: { state: StoreService; namespace: string | null }) {
  const { rows } = state;
  const { lens, privateDns, runtime, replicas, status } = useServiceStatus(state, namespace);
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

/** The check that runs now: its deployed value while the next Deploy changes it. */
export const deployedHealthcheck = ({ changes, rows }: Pick<StoreService, "changes" | "rows">) =>
  healthcheckOf(changes.has("healthcheck") ? changes.get("healthcheck")?.before : rows.get("healthcheck")?.value);

/** The card's healthcheck ⚠, in the panel, while a replica that passed its check is failing it now. */
export function HealthcheckFailing({ state, namespace }: { state: StoreService; namespace: string | null }) {
  const { status } = useServiceStatus(state, namespace);
  return status.checkFailing ? <HealthcheckWarning check={deployedHealthcheck(state)} /> : null;
}

export function HealthcheckWarning({ check }: { check: Healthcheck | null }) {
  return (
    <Item variant="outline" state="warning" size="sm" role="note">
      <ItemMedia><TriangleAlertIcon className="text-warning" /></ItemMedia>
      <ItemContent>
        <ItemTitle>Healthcheck failing</ItemTitle>
        {check ? <ItemDescription className="font-mono">{check.text}</ItemDescription> : null}
      </ItemContent>
    </Item>
  );
}
