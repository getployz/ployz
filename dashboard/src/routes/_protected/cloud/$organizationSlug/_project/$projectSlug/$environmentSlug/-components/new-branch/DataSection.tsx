import { TriangleAlertIcon } from "lucide-react";
import { Badge } from "#/components/ui/badge";
import { Field, FieldDescription, FieldLabel, FieldLegend, FieldSet } from "#/components/ui/field";
import { Item, ItemContent, ItemDescription, ItemMedia } from "#/components/ui/item";
import { RadioGroup, RadioGroupItem } from "#/components/ui/radio-group";
import type { SavedEnvironmentIntent } from "#/modules/environment-design/saved-intent";
import { listNames, type BranchPlan } from "#/modules/branches/branch-plan";

/**
 * Where the Branch's data comes from: Own Copies of Volumes start empty (copying data is Soon), and a Live Node that owns
 * data is the Parent's real data.
 */
export function DataSection({ intent, plan, parentName, rootName, nameOf }: {
  intent: SavedEnvironmentIntent;
  plan: BranchPlan;
  parentName: string;
  /** The root the Parent was branched from, when the Parent is itself a Branch. */
  rootName: string | null;
  nameOf: (lineage: string) => string;
}) {
  const own = plan.nodes.filter((node) => node.role === "own" && node.nodeType === "volume").map((node) => nameOf(node.lineageId));
  const liveData = plan.nodes.filter((node) => node.role === "live"
    && intent.services.some((service) => service.lineageId === node.lineageId && service.volumeAttachments.length > 0))
    .map((node) => nameOf(node.lineageId));
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
      </>}
      {liveData.length > 0 && (
        <Item variant="outline" state="warning" size="sm" role="note">
          <ItemMedia><TriangleAlertIcon className="text-warning" /></ItemMedia>
          <ItemContent>
            <ItemDescription className="text-foreground">
              {listNames(liveData)} {liveData.length === 1 ? "is" : "are"} {parentName}'s, live. This branch reads and writes {liveData.length === 1 ? "its" : "their"} real data.
            </ItemDescription>
          </ItemContent>
        </Item>
      )}
      {own.length === 0 && liveData.length === 0 && <FieldDescription>Nothing here keeps data.</FieldDescription>}
    </FieldSet>
  );
}
