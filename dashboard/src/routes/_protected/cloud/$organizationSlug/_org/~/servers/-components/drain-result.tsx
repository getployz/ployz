import { BoxIcon, CircleCheckIcon, CircleDashedIcon, CircleMinusIcon, CircleStopIcon, CircleXIcon, GlobeIcon, type LucideIcon } from "lucide-react";
import { Item, ItemActions, ItemContent, ItemDescription, ItemGroup, ItemMedia, ItemTitle } from "#/components/ui/item";
import { cn } from "#/lib/utils";
import type { DrainRow, DrainTone } from "#/modules/machines/server-drain-view";

const TONE_CLASS = {
  moved: "text-success",
  stopped: "text-muted-foreground",
  stayed: "text-warning",
  failed: "text-destructive",
  neutral: "text-muted-foreground",
} satisfies Record<DrainTone, string>;
const TONE_ICON = {
  moved: CircleCheckIcon,
  stopped: CircleStopIcon,
  stayed: CircleMinusIcon,
  failed: CircleXIcon,
  neutral: CircleDashedIcon,
} satisfies Record<DrainTone, LucideIcon>;

/**
 * Each Service and what the latest Drain did with it. Below the navigation breakpoint the outcome wraps onto its own
 * line, so a long reason never squeezes it.
 */
export function DrainResultList({ rows }: { rows: readonly DrainRow[] }) {
  if (rows.length === 0) return null;
  return (
    <ItemGroup aria-label="Drain result">
      {rows.map((row) => {
        const Icon = TONE_ICON[row.tone];
        return (
          <Item key={row.key} variant="outline" size="sm" data-row={row.key}>
            <ItemMedia variant="icon" className="self-start">{row.global ? <GlobeIcon /> : <BoxIcon />}</ItemMedia>
            <ItemContent className="min-w-0">
              <ItemTitle>{row.name}</ItemTitle>
              <ItemDescription className={row.tone === "failed" ? "text-destructive" : undefined}>
                {row.reason ?? row.namespace}
              </ItemDescription>
            </ItemContent>
            <ItemActions className={cn("max-wf-nav:basis-full", TONE_CLASS[row.tone])}>
              <Icon aria-hidden className="size-4 shrink-0" />
              <span>{row.label}</span>
            </ItemActions>
          </Item>
        );
      })}
    </ItemGroup>
  );
}
