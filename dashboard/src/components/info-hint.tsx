import type { ReactNode } from "react";
import { InfoIcon } from "lucide-react";
import { Button } from "#/components/ui/button";
import { Popover, PopoverContent, PopoverTrigger } from "#/components/ui/popover";

/**
 * The why behind a short line, one tap away: an ⓘ that opens on click, tap or keyboard, and on hover where there is one.
 * The line stays short; the explanation lives here.
 */
export function InfoHint({ label = "Why", children }: { label?: string; children: ReactNode }) {
  return (
    <Popover>
      <PopoverTrigger openOnHover render={<Button type="button" variant="ghost" size="icon-xs" aria-label={label} />}>
        <InfoIcon />
      </PopoverTrigger>
      <PopoverContent className="text-sm">{children}</PopoverContent>
    </Popover>
  );
}
