import { SquareCheckIcon, SquareIcon } from "lucide-react";
import { FieldContent, FieldDescription, FieldLabel, FieldLegend, FieldSet, FieldTitle, Field } from "#/components/ui/field";
import { Item, ItemContent, ItemDescription, ItemGroup, ItemMedia, ItemTitle } from "#/components/ui/item";
import { RadioGroup, RadioGroupItem } from "#/components/ui/radio-group";
import { cn } from "#/lib/utils";
import { listNames, presetSummary, presetTitles, type BranchPlan, type BranchPreset } from "#/modules/branches/branch-plan";

type PlanNode = BranchPlan["nodes"][number];

function roleText(node: PlanNode, parentName: string, ownerName: (lineage: string) => string, fromPr: ReadonlySet<string>) {
  if (node.role === "live") return `${ownerName(node.lineageId)}'s, live`;
  if (node.role === "left_out") return "left out";
  if (fromPr.has(node.lineageId)) return "own copy · from the PR";
  if (node.because === "used") return "own copy · a copy uses it";
  if (node.because === "parent_not_deployed") return `own copy · ${parentName} never deployed it`;
  return "own copy";
}

/**
 * The presets, each saying what it will do, then how to pick by hand: on desktop by clicking cards on the canvas (a legend
 * says what the canvas shows), on phones with a tick list of the Parent's nodes. Picking toggles an Own Copy; what you pick
 * is what changes. A node another copy needs, one the Parent uses live, or one running the pull request's code can't be toggled.
 */
export function WhatComesAlongSection({ parentName, ownerName, plan, presets, nameOf, fixed, fromPr, footnote, onPreset, onToggle }: {
  parentName: string;
  /** The Environment a Live Node comes from. */
  ownerName: (lineage: string) => string;
  plan: BranchPlan;
  presets: Array<{ preset: BranchPreset; plan: BranchPlan }>;
  nameOf: (lineage: string) => string;
  /** Clicking changes nothing. */
  fixed: (node: PlanNode) => boolean;
  /** Own Copies running the pull request's code. */
  fromPr: ReadonlySet<string>;
  /** A line under the presets. */
  footnote?: string;
  onPreset: (preset: BranchPreset) => void;
  onToggle: (lineage: string) => void;
}) {
  const only = presets.find((option) => option.preset === "only")?.plan ?? plan;
  const owners = [...new Set(plan.nodes.filter((node) => node.role === "live").map((node) => ownerName(node.lineageId)))];
  return (
    <FieldSet>
      <FieldLegend>What comes along</FieldLegend>
      <RadioGroup value={plan.preset} onValueChange={(value) => {
        const chosen = presets.find((option) => option.preset === value);
        if (chosen) onPreset(chosen.preset);
      }}>
        {presets.map((option) => (
          <FieldLabel key={option.preset} htmlFor={`branch-preset-${option.preset}`}>
            <Field orientation="horizontal">
              <RadioGroupItem value={option.preset} id={`branch-preset-${option.preset}`} />
              <FieldContent>
                <FieldTitle>{presetTitles[option.preset]}</FieldTitle>
                <FieldDescription>{presetSummary(option.preset, option.plan, only, nameOf, parentName)}</FieldDescription>
              </FieldContent>
            </Field>
          </FieldLabel>
        ))}
      </RadioGroup>
      {footnote && <FieldDescription>{footnote}</FieldDescription>}
      <ul aria-label="Legend" className="hidden flex-wrap gap-x-4 gap-y-1 text-xs text-muted-foreground min-[861px]:flex">
        <li className="flex items-center gap-1.5"><span aria-hidden="true" className="h-2.5 w-4 rounded-sm border-2 border-foreground" />Own copy</li>
        <li className="flex items-center gap-1.5"><span aria-hidden="true" className="h-2.5 w-4 rounded-sm border border-dashed border-muted-foreground" />{listNames(owners.length ? owners : [parentName])}'s, live</li>
        <li className="flex items-center gap-1.5"><span aria-hidden="true" className="h-2.5 w-4 rounded-sm border opacity-50" />Left out</li>
      </ul>
      <FieldDescription>
        {plan.preset === null ? "Picked by hand. " : ""}
        <span className="min-[861px]:hidden">Tick what gets its own copy.</span>
        <span className="hidden min-[861px]:inline">Click cards on the canvas to pick.</span>
      </FieldDescription>
      <ItemGroup className="gap-1 min-[861px]:hidden">
        {plan.nodes.map((node) => {
          const isOwn = node.role === "own";
          const locked = fixed(node);
          return (
            <Item key={node.lineageId} size="xs" variant="outline"
              render={<button type="button" aria-pressed={isOwn} disabled={locked} onClick={() => onToggle(node.lineageId)} />}
              className={cn("text-left", locked ? "cursor-default" : "hover:bg-muted")}>
              <ItemMedia className={cn(isOwn ? "text-foreground" : "text-muted-foreground", locked && "opacity-50")}>
                {isOwn ? <SquareCheckIcon aria-hidden="true" /> : <SquareIcon aria-hidden="true" />}
              </ItemMedia>
              <ItemContent>
                <ItemTitle>{nameOf(node.lineageId)}</ItemTitle>
                <ItemDescription>{roleText(node, parentName, ownerName, fromPr)}</ItemDescription>
              </ItemContent>
            </Item>
          );
        })}
      </ItemGroup>
    </FieldSet>
  );
}
