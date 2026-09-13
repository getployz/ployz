import type { ReactNode } from "react";
import { SearchIcon } from "lucide-react";
import {
  InputGroup,
  InputGroupAddon,
  InputGroupInput,
  InputGroupText,
} from "#/components/ui/input-group";

type ResourcePageControlsProps = {
  action?: ReactNode;
  controlsLabel: string;
  searchAriaLabel: string;
  searchPlaceholder: string;
  searchValue: string;
  onSearchValueChange: (value: string) => void;
};

export function ResourcePageControls({
  action,
  controlsLabel,
  searchAriaLabel,
  searchPlaceholder,
  searchValue,
  onSearchValueChange,
}: ResourcePageControlsProps) {
  return (
    <section aria-label={controlsLabel} className="flex items-center gap-3">
      <InputGroup className="flex-1">
        <InputGroupInput
          aria-label={searchAriaLabel}
          placeholder={searchPlaceholder}
          value={searchValue}
          onChange={(event) => onSearchValueChange(event.target.value)}
        />
        <InputGroupAddon align="inline-start">
          <InputGroupText>
            <SearchIcon />
          </InputGroupText>
        </InputGroupAddon>
      </InputGroup>

      {action}
    </section>
  );
}
