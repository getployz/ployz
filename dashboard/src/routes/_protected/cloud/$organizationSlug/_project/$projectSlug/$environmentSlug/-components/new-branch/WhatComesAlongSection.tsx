import { SquareCheckIcon, SquareIcon } from "lucide-react";
import { FieldContent, FieldDescription, FieldLabel, FieldLegend, FieldSet, FieldTitle, Field } from "#/components/ui/field";
import { Item, ItemContent, ItemDescription, ItemGroup, ItemMedia, ItemTitle } from "#/components/ui/item";
import { RadioGroup, RadioGroupItem } from "#/components/ui/radio-group";
import { cn } from "#/lib/utils";
import { presetSummary, type BranchPlan, type BranchPreset } from "#/modules/branches/branch-plan";

const presetTitles = {
  only: "Only what changes",
  uses: "Plus what it uses",
  all: "Everything",
} satisfies Record<BranchPreset, string>;

type PlanNode = BranchPlan["nodes"][number];

function roleText(node: PlanNode, parentName: string) {
  if (node.role === "live") return `${parentName}'s, live`;
  if (node.role === "left_out") return "left out";
  if (node.because === "used") return "own copy · a copy uses it";
  if (node.because === "parent_not_deployed") return `own copy · ${parentName} never deployed it`;
  return "own copy";
}

/**
 * The presets, each saying what it will do, and a tick list of the Parent's nodes saying what each becomes. Ticking toggles
 * an Own Copy; what you tick is what changes. A node another copy needs, or one the Parent uses live, can't be toggled.
 */
export function WhatComesAlongSection({ parentName, plan, presets, nameOf, owned, onPreset, onToggle }: {
  parentName: string;
  plan: BranchPlan;
  presets: Array<{ preset: BranchPreset; plan: BranchPlan }>;
  nameOf: (lineage: string) => string;
  owned: ReadonlySet<string>;
  onPreset: (preset: BranchPreset) => void;
  onToggle: (lineage: string) => void;
}) {
  const only = presets.find((option) => option.preset === "only")?.plan ?? plan;
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
      <FieldDescription>{plan.preset === null ? "Picked by hand. " : ""}Tick what gets its own copy.</FieldDescription>
      <ItemGroup className="gap-1">
        {plan.nodes.map((node) => {
          const isOwn = node.role === "own";
          const fixed = !owned.has(node.lineageId) || (isOwn && node.because !== "picked");
          return (
            <Item key={node.lineageId} size="xs" variant="outline"
              render={<button type="button" aria-pressed={isOwn} disabled={fixed} onClick={() => onToggle(node.lineageId)} />}
              className={cn("text-left", fixed ? "cursor-default" : "hover:bg-muted")}>
              <ItemMedia className={cn(isOwn ? "text-foreground" : "text-muted-foreground", fixed && "opacity-50")}>
                {isOwn ? <SquareCheckIcon aria-hidden="true" /> : <SquareIcon aria-hidden="true" />}
              </ItemMedia>
              <ItemContent>
                <ItemTitle>{nameOf(node.lineageId)}</ItemTitle>
                <ItemDescription>{roleText(node, parentName)}</ItemDescription>
              </ItemContent>
            </Item>
          );
        })}
      </ItemGroup>
    </FieldSet>
  );
}
