import type { ReactNode } from "react";
import { XIcon } from "lucide-react";
import { Button } from "#/components/ui/button";
import { Textarea } from "#/components/ui/textarea";

/** Where a step or an act runs: the Namespace and Runtime that name its Servers. */
export function ServerLine({ namespace, runtime }: { namespace: string; runtime: string }) {
  return <span className="block text-xs text-muted-foreground">On servers <code className="font-mono">{namespace}/{runtime}</code></span>;
}

/** What a decision destroys, one `DestroyedItem` each. */
export function DestroysList({ label, children }: { label: string; children: ReactNode }) {
  return (
    <ul aria-label={label} className="flex flex-col gap-1 rounded-lg bg-destructive/10 px-2.5 py-2 text-xs">
      {children}
    </ul>
  );
}

export function DestroyedItem({ children }: { children: ReactNode }) {
  return (
    <li className="flex gap-2">
      <XIcon aria-hidden className="mt-0.5 size-3 shrink-0 text-destructive" />
      <span>{children}</span>
    </li>
  );
}

/** The box a denial's reason goes in, which the agent reads. */
export function DenyReason({ value, onChange }: { value: string; onChange: (reason: string) => void }) {
  return (
    <Textarea autoFocus aria-label="Reason" placeholder="Why not? The agent reads this." value={value}
      onChange={(event) => onChange(event.target.value)} className="min-h-14 text-xs" />
  );
}

export function DenyWithReasonButton({ onClick }: { onClick: () => void }) {
  return (
    <Button size="sm" variant="ghost" className="ml-auto text-muted-foreground" onClick={onClick}>
      Deny with a reason…
    </Button>
  );
}

/** A denial's body: the reason when the member wrote one, else none. */
export const denial = (reason: string | null) => reason?.trim() ? { reject: { reason: reason.trim() } } : { reject: {} };
