import type { ReactNode } from "react";
import { ArrowLeftIcon } from "lucide-react";
import { InputGroup, InputGroupAddon, InputGroupButton } from "#/components/ui/input-group";

export function SourcePickerLayout({ title, children }: { title: string; children: ReactNode }) {
  return (
    <div className="flex min-w-0 flex-col gap-3">
      <div className="w-fit rounded-md border bg-popover px-3 py-2 font-medium">{title}</div>
      <div className="flex min-w-0 flex-col gap-3 rounded-xl bg-popover p-3 text-popover-foreground [&>[data-slot=command]]:overflow-visible">
        {children}
      </div>
    </div>
  );
}

export function SourcePickerInput({ children, onBack, disabled }: {
  children: ReactNode;
  onBack?: () => void;
  disabled?: boolean;
}) {
  return (
    <InputGroup>
      {onBack ? (
        <InputGroupAddon>
          <InputGroupButton type="button" aria-label="Back" size="icon-sm" onClick={onBack} disabled={disabled}>
            <ArrowLeftIcon />
          </InputGroupButton>
        </InputGroupAddon>
      ) : null}
      {children}
    </InputGroup>
  );
}
