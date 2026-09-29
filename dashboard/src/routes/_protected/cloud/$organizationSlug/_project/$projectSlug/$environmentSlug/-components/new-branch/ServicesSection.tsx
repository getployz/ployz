import { useState, type ReactNode } from "react";
import { ArrowLeftIcon, ArrowRightIcon, ChevronRightIcon, TriangleAlertIcon } from "lucide-react";
import { Dialog, DialogContent, DialogHeader, DialogTitle } from "#/components/ui/dialog";
import { FieldDescription, FieldLegend, FieldSet } from "#/components/ui/field";
import { Item, ItemContent, ItemDescription, ItemGroup, ItemTitle } from "#/components/ui/item";
import { ToggleGroup, ToggleGroupItem } from "#/components/ui/toggle-group";
import { cn } from "#/lib/utils";
import { presetTitles, type BranchPlan } from "#/modules/config-store/branch-picks";
import { nodePick, type PickingView } from "./branch-picking";

type PlanNode = BranchPlan["nodes"][number];

/**
 * What the Branch runs, next to its Parent: one row per Parent node, the Parent's on the left and the Branch's on the
 * right. Tapping a row opens a sheet with its two choices, one line each; a Live Node with real data is amber.
 */
export function ServicesSection({ picking, nameOf, target, who }: {
  picking: PickingView;
  nameOf: (lineage: string) => string;
  /** The right column's heading: the branch's name, or "each pull request". */
  target: ReactNode;
  /** Who can change a Live Node's real data: "This branch", "The PR". */
  who: string;
}) {
  const [choosing, setChoosing] = useState<string | null>(null);
  const { plan, parent } = picking;
  const chosen = plan.nodes.find((node) => node.lineageId === choosing);
  const toggled = chosen && picking.toggled(chosen.lineageId);

  const choice = (node: PlanNode) => {
    const name = nameOf(node.lineageId);
    const owner = picking.ownerName(node.lineageId);
    if (node.role === "live") {
      const data = nodePick(picking, node).ownsData;
      return { title: `${owner}'s ${name}`, line: data ? `Real data. ${who} can change it.` : `Shared with ${owner}.`, data };
    }
    if (node.role === "left_out") return { title: "Leave it out", line: "Anything that uses it breaks.", data: false };
    if (picking.fromPr.has(node.lineageId)) return { title: "Run the PR's code", line: "What you're testing.", data: false };
    return node.nodeType === "volume"
      ? { title: `New, empty ${name}`, line: "Starts with no data.", data: false }
      : { title: `Separate ${name}`, line: `Change it here without touching ${parent.name}.`, data: false };
  };

  return (
    <FieldSet>
      <FieldLegend>Services</FieldLegend>
      <FieldDescription>What runs next to {parent.name}. Tap one to change it.</FieldDescription>
      {picking.presets.length > 1 && (
        <ToggleGroup variant="outline" size="sm" spacing={0} className="w-full" value={plan.preset ? [plan.preset] : []}
          onValueChange={([value]) => {
            const chosen = picking.presets.find(({ preset }) => preset === value);
            if (chosen) picking.setPreset(chosen.preset);
          }}>
          {picking.presets.map(({ preset }) => (
            <ToggleGroupItem key={preset} value={preset} className="flex-1">{presetTitles[preset]}</ToggleGroupItem>
          ))}
        </ToggleGroup>
      )}
      <ItemGroup className="gap-2">
        <div className="grid grid-cols-[minmax(0,1fr)_1.5rem_minmax(0,1.4fr)] gap-1 text-xs text-muted-foreground">
          <span className="truncate">{parent.name}</span>
          <span />
          <span className="flex min-w-0 items-center gap-1.5 truncate">{target}</span>
        </div>
        {plan.nodes.map((node) => {
          const pick = nodePick(picking, node);
          const name = nameOf(node.lineageId);
          const why = node.role !== "own" || picking.fromPr.has(node.lineageId) ? null
            : node.because === "used" ? "a copy uses it" : node.because === "parent_not_deployed" ? `${parent.name} never deployed it` : null;
          const Arrow = node.role === "own" ? ArrowRightIcon : node.role === "live" ? ArrowLeftIcon : null;
          return (
            <div key={node.lineageId} className="grid grid-cols-[minmax(0,1fr)_1.5rem_minmax(0,1.4fr)] items-center gap-1">
              <Item variant="muted" size="sm" className="h-full">
                <ItemContent className="min-w-0"><ItemTitle className="truncate">{name}</ItemTitle></ItemContent>
              </Item>
              {Arrow ? <Arrow aria-hidden="true" className={cn("mx-auto size-4", pick.ownsData ? "text-warning" : "text-muted-foreground")} /> : <span />}
              <Item variant="outline" size="sm" state={pick.ownsData ? "warning" : undefined}
                render={<button type="button" disabled={pick.fixed} onClick={() => setChoosing(node.lineageId)}
                  aria-label={`${name}: ${pick.label}${pick.ownsData ? ", real data" : ""}. Change`} />}
                className={cn("h-full text-left", node.role !== "own" && "border-dashed", node.role === "left_out" && "opacity-50",
                  !pick.fixed && "hover:bg-muted")}>
                <ItemContent className="min-w-0">
                  <ItemTitle className="w-full truncate">{name}</ItemTitle>
                  <ItemDescription className={cn("truncate", pick.ownsData && "text-warning")}>
                    {pick.ownsData && <TriangleAlertIcon aria-hidden="true" className="mr-1 inline size-3" />}
                    {pick.label}{why && ` · ${why}`}
                  </ItemDescription>
                </ItemContent>
                {!pick.fixed && <ChevronRightIcon aria-hidden="true" className="size-4 text-muted-foreground" />}
              </Item>
            </div>
          );
        })}
      </ItemGroup>
      <Dialog open={chosen !== undefined} onOpenChange={(open) => { if (!open) setChoosing(null); }}>
        <DialogContent>
          <DialogHeader><DialogTitle>{chosen && nameOf(chosen.lineageId)}</DialogTitle></DialogHeader>
          <ItemGroup className="gap-2">
            {chosen && toggled && (chosen.role === "own" ? [chosen, toggled] : [toggled, chosen]).map((node) => {
              const { title, line, data } = choice(node);
              const current = node === chosen;
              return (
                <Item key={node.role} variant="outline" aria-pressed={current}
                  render={<button type="button" autoFocus={current} onClick={() => {
                    if (!current) picking.toggle(node.lineageId);
                    setChoosing(null);
                  }} />}
                  className={cn("text-left hover:bg-muted", current && "border-foreground")}>
                  <ItemContent>
                    <ItemTitle>{title}</ItemTitle>
                    <ItemDescription className={cn(data && "text-warning")}>
                      {data && <TriangleAlertIcon aria-hidden="true" className="mr-1 inline size-3" />}{line}
                    </ItemDescription>
                  </ItemContent>
                </Item>
              );
            })}
          </ItemGroup>
        </DialogContent>
      </Dialog>
    </FieldSet>
  );
}
