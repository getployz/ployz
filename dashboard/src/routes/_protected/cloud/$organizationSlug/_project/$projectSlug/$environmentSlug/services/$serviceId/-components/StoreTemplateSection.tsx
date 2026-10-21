import { Link, useParams } from "@tanstack/react-router";
import { LayersIcon } from "lucide-react";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import { Field, FieldContent, FieldDescription, FieldGroup, FieldLabel } from "#/components/ui/field";
import type { EditedRow, InstallView } from "#/modules/config-store/templates";
import { fieldName, shown } from "../../../-components/TemplateUpdateDialog";
import { ENVIRONMENT_ROUTE_FROM, ENVIRONMENT_TEMPLATE_ROUTE_TO } from "../../../-components/environment-route-paths";
import type { StoreService } from "./StoreServiceDrawer";

/** Where an edit came from, in plain words. */
export const originText = (row: Pick<EditedRow, "origin">, parent: string | null) =>
  row.origin === "delivered" ? `From ${parent ?? "another environment"}` : "Changed here";

/** The Install a Service is part of, and each of its rows changed away from it, with where the change came from. */
export function StoreTemplateSection({ state, instance, parent }: { state: StoreService; instance: InstallView; parent: string | null }) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const { template, install, release } = instance;
  const edited = instance.edited.filter((row) => row.service === state.service.name);
  return (
    <FieldGroup>
      <Link to={ENVIRONMENT_TEMPLATE_ROUTE_TO} params={{ ...params, templateId: install.name }} search={{}}
        className="flex items-center gap-2 text-sm font-medium hover:underline">
        <LayersIcon className="size-4" />{install.name}<span className="font-normal text-muted-foreground">{template.name} v{release.version}</span>
      </Link>
      {edited.map((row) => (
        <Field key={row.path} orientation="horizontal">
          <FieldContent>
            <FieldLabel>{fieldName(row)}<Badge variant="outline">{originText(row, parent)}</Badge></FieldLabel>
            <FieldDescription className="truncate">Template sets {shown(row, row.base)}</FieldDescription>
          </FieldContent>
          <Button type="button" variant="outline" size="sm" onClick={() => state.edit({ op: "set", path: row.path, value: row.base })}>
            Reset
          </Button>
        </Field>
      ))}
    </FieldGroup>
  );
}
