import { useState } from "react";
import type { EnvironmentView, ServiceListing, VolumeListing } from "@ployz/sdk";
import { ArrowRightIcon } from "lucide-react";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import { Dialog, DialogClose, DialogContent, DialogFooter, DialogHeader, DialogTitle } from "#/components/ui/dialog";
import { Field, FieldContent, FieldLabel } from "#/components/ui/field";
import { cn } from "#/lib/utils";
import { ToggleGroup, ToggleGroupItem } from "#/components/ui/toggle-group";
import { settingTitle } from "#/modules/config-store/catalog";
import { planInstall, type Decision, type Install, type InstallPlan, type InstallView, type MergeRow } from "#/modules/config-store/templates";

/** An image reads as its tag; anything else as itself. */
export const shown = (row: Pick<MergeRow, "field">, value: string | number | null) => {
  const text = String(value ?? "");
  return row.field === "image" ? text.slice(text.lastIndexOf(":") + 1) : text || "unset";
};

/** A field reads as its Setting's title, a variable as its key. */
export const fieldName = ({ field }: Pick<MergeRow, "field">) =>
  field.startsWith("env.") ? field.slice(4) : settingTitle(field) ?? field;

function Row({ row, from, to, badge }: { row: MergeRow; from: string | null; to: string | null; badge?: React.ReactNode }) {
  return (
    <div className="grid grid-cols-[minmax(0,10rem)_minmax(0,1fr)_auto] items-center gap-3 text-sm">
      <span className={cn("truncate text-xs text-muted-foreground", row.field.startsWith("env.") && "font-mono")}>{row.service} · {fieldName(row)}</span>
      <span className="flex min-w-0 items-center gap-1.5 font-mono text-xs">
        {from !== null && <><span className="truncate text-muted-foreground line-through">{shown(row, from)}</span><ArrowRightIcon className="size-3 shrink-0 text-muted-foreground" /></>}
        <span className="truncate">{shown(row, to)}</span>
      </span>
      {badge ?? <span />}
    </div>
  );
}

function Group({ title, children }: { title: string; children: React.ReactNode }) {
  return <div className="flex flex-col gap-1.5"><div className="text-sm font-medium">{title}</div>{children}</div>;
}

/**
 * What updating to the newest release does, merged against what this Environment changed: rows the release changes,
 * edits that stay, and edits to rows the release drops, which each need Keep edit or Remove. Left undecided, they block
 * Deploy until picked in the panel.
 */
export function TemplateUpdateDialog({ instance, services, settings, volumes, onClose, onConfirm }: {
  instance: InstallView;
  services: readonly ServiceListing[];
  settings: EnvironmentView;
  volumes: readonly VolumeListing[];
  onClose: () => void;
  onConfirm: (plan: InstallPlan, decisions: Record<string, Decision>) => void;
}) {
  const { template, install, latest } = instance;
  const [picked, setPicked] = useState<Record<string, string>>({});
  const [decisions, setDecisions] = useState<Record<string, Decision>>({});
  if (!latest) return null;
  const asks = latest.options.flatMap((option) =>
    option.kind === "choice" && option.default === null && install.inputs[option.key]?.state !== "value" ? [option] : []);
  const inputs: Install["inputs"] = { ...install.inputs,
    ...Object.fromEntries(Object.entries(picked).map(([key, value]) => [key, { state: "value", value } as const])) };
  const plan = planInstall(instance, latest.version, inputs, services, settings);
  const hasData = (service: string) => volumes.some((volume) => volume.mounts.some((mount) => mount.service === service));

  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="max-h-[90dvh] overflow-y-auto sm:max-w-2xl">
        <DialogHeader><DialogTitle>Update {install.name} to {template.name} v{latest.version}</DialogTitle></DialogHeader>
        <div className="flex flex-col gap-5">
          {asks.map((option) => (
            <Field key={option.key} orientation="responsive">
              <FieldContent><FieldLabel>{option.label}{!picked[option.key] && <Badge variant="warning">Not set</Badge>}</FieldLabel></FieldContent>
              <ToggleGroup variant="outline" size="sm" spacing={0} aria-label={option.label}
                value={picked[option.key] ? [picked[option.key] ?? ""] : []}
                onValueChange={([value]) => { if (value) setPicked({ ...picked, [option.key]: value }); }}>
                {Object.entries(option.choices).map(([value, label]) => <ToggleGroupItem key={value} value={value}>{label}</ToggleGroupItem>)}
              </ToggleGroup>
            </Field>
          ))}
          {plan.changes.length > 0 && (
            <Group title="Changes">
              {plan.changes.map((row) => (
                <Row key={row.path} row={row} from={row.here} to={row.target}
                  badge={row.field === "image" && hasData(row.service) ? <Badge variant="warning">Has data</Badge> : undefined} />
              ))}
            </Group>
          )}
          {plan.kept.length > 0 && (
            <Group title="Kept">
              {plan.kept.map((row) => <Row key={row.path} row={row} from={null} to={row.here} badge={<Badge variant="outline">Edited</Badge>} />)}
            </Group>
          )}
          {plan.decide.length > 0 && (
            <Group title={`Dropped in v${latest.version}`}>
              {plan.decide.map((row) => (
                <Row key={row.path} row={row} from={null} to={row.here} badge={
                  <ToggleGroup variant="outline" size="sm" spacing={0} aria-label={`${row.service} ${fieldName(row)}`}
                    value={decisions[row.path] ? [decisions[row.path] ?? ""] : []}
                    onValueChange={([value]) => { if (value === "keep" || value === "remove") setDecisions({ ...decisions, [row.path]: value }); }}>
                    <ToggleGroupItem value="keep">Keep edit</ToggleGroupItem>
                    <ToggleGroupItem value="remove">Remove</ToggleGroupItem>
                  </ToggleGroup>
                } />
              ))}
            </Group>
          )}
          {plan.added.map((member) => (
            <div key={member.name} className="flex items-center gap-2 text-sm font-medium">{member.name}<Badge variant="changed">Added</Badge></div>
          ))}
          {plan.removed.map((service) => (
            <div key={service.id} className="flex items-center gap-2 text-sm font-medium">{service.name}<Badge variant="outline">Removed</Badge></div>
          ))}
        </div>
        <DialogFooter>
          <DialogClose render={<Button type="button" variant="outline" />}>Cancel</DialogClose>
          <Button type="button" onClick={() => onConfirm(plan, decisions)}>Update</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
