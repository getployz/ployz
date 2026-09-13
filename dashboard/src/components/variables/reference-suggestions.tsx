import { LockIcon } from "lucide-react";
import { Badge } from "#/components/ui/badge";
import type { ReferenceTarget } from "#/modules/environment-design/variable-autocomplete";
import { cn } from "#/lib/utils";

/**
 * The anchored `${{ }}` suggestion list shared by the value input and textarea.
 * Rendered inside a `relative` wrapper; keeps focus in the field (selection via
 * `onMouseDown` + `preventDefault`).
 */
export function ReferenceSuggestionList({
  suggestions,
  activeIndex,
  onSelect,
  onActiveIndexChange,
}: {
  suggestions: ReferenceTarget[];
  activeIndex: number;
  onSelect: (target: ReferenceTarget) => void;
  onActiveIndexChange: (index: number) => void;
}) {
  return (
    <ul className="absolute top-full left-0 z-50 mt-1 max-h-64 w-full overflow-y-auto rounded-md border bg-popover p-1 text-popover-foreground shadow-md">
      {suggestions.map((target, index) => (
        <li key={`${target.ownerSlug ?? "self"}:${target.key}`}>
          <button
            type="button"
            onMouseDown={(event) => {
              event.preventDefault();
              onSelect(target);
            }}
            onMouseEnter={() => onActiveIndexChange(index)}
            className={cn(
              "flex w-full items-center gap-2 rounded-sm px-2 py-1.5 text-left",
              index === activeIndex && "bg-muted",
            )}
          >
            <span className="truncate font-mono text-xs">
              {target.ownerSlug ? `${target.ownerSlug}.` : ""}
              {target.key}
            </span>
            {target.isSecret ? (
              <LockIcon className="size-3 shrink-0 text-muted-foreground" />
            ) : null}
            <Badge variant="secondary" className="ml-auto shrink-0">
              {target.ownerLabel}
            </Badge>
          </button>
        </li>
      ))}
    </ul>
  );
}
