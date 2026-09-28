import { TriangleAlertIcon } from "lucide-react";
import { Badge } from "#/components/ui/badge";
import { Field, FieldDescription, FieldLabel, FieldLegend, FieldSet } from "#/components/ui/field";
import { Item, ItemContent, ItemDescription, ItemMedia } from "#/components/ui/item";
import { RadioGroup, RadioGroupItem } from "#/components/ui/radio-group";
import type { SetupCommand } from "#/modules/project/tables";
import { listNames, type BranchPlan } from "#/modules/branches/branch-plan";
import type { LiveNodeOwner } from "#/modules/branches/use-live-nodes";
import { SetupCommandsField } from "./SetupCommandsField";

/**
 * Where the Branch's data comes from: Own Copies of Volumes start empty (copying data is Soon), and a Live Node that owns
 * data, by its owner's Applied State, is that owner's real data. Setup Commands then run in the Branch's own services.
 */
export function DataSection({ plan, liveOwner, parentName, rootName, nameOf, setupCommands, onSetupCommands, onSetupBlur, setupHelp }: {
  plan: BranchPlan;
  liveOwner: (lineage: string) => LiveNodeOwner | null;
  parentName: string;
  /** The root the Parent was branched from, when the Parent is itself a Branch. */
  rootName: string | null;
  nameOf: (lineage: string) => string;
  setupCommands: SetupCommand[];
  onSetupCommands: (next: SetupCommand[], typed?: boolean) => void;
  onSetupBlur?: () => void;
  setupHelp?: string;
}) {
  const own = plan.nodes.filter((node) => node.role === "own" && node.nodeType === "volume").map((node) => nameOf(node.lineageId));
  // Each owner's Live Nodes that keep data, by owner name.
  const liveData = new Map<string, string[]>();
  for (const node of plan.nodes) {
    const owner = node.role === "live" ? liveOwner(node.lineageId) : null;
    if (owner?.ownsData) liveData.set(owner.environment.name, [...liveData.get(owner.environment.name) ?? [], nameOf(node.lineageId)]);
  }
  const ownServices = plan.nodes.filter((node) => node.role === "own" && node.nodeType === "service")
    .map((node) => ({ lineageId: node.lineageId, name: nameOf(node.lineageId) }));
  const soon = [parentName, ...(rootName ? [rootName] : [])];
  return (
    <FieldSet>
      <FieldLegend>Data</FieldLegend>
      {own.length > 0 && <>
        <FieldDescription>Where {listNames(own)} {own.length === 1 ? "starts" : "start"} from.</FieldDescription>
        <RadioGroup value="empty">
          <FieldLabel htmlFor="branch-data-empty">
            <Field orientation="horizontal"><RadioGroupItem value="empty" id="branch-data-empty" />Start empty</Field>
          </FieldLabel>
          {soon.map((source) => (
            <FieldLabel key={source} htmlFor={`branch-data-${source}`}>
              <Field orientation="horizontal" data-disabled="true">
                <RadioGroupItem value={source} id={`branch-data-${source}`} disabled />
                Copy {source}'s data<Badge variant="secondary">Soon</Badge>
              </Field>
            </FieldLabel>
          ))}
        </RadioGroup>
        {ownServices.length > 0 && (
          <Field>
            <FieldLabel htmlFor="branch-setup-command">Then run</FieldLabel>
            <SetupCommandsField id="branch-setup-command" commands={setupCommands} services={ownServices} onChange={onSetupCommands} onBlur={onSetupBlur} />
            <FieldDescription>{setupHelp ?? "Runs once, before the service first starts."}</FieldDescription>
          </Field>
        )}
      </>}
      {[...liveData].map(([owner, names]) => (
        <Item key={owner} variant="outline" state="warning" size="sm" role="note">
          <ItemMedia><TriangleAlertIcon className="text-warning" /></ItemMedia>
          <ItemContent>
            <ItemDescription className="text-foreground">
              This branch writes to {owner}'s {listNames(names)}.
            </ItemDescription>
          </ItemContent>
        </Item>
      ))}
      {own.length === 0 && liveData.size === 0 && <FieldDescription>Nothing here keeps data.</FieldDescription>}
    </FieldSet>
  );
}
