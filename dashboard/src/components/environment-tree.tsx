import { GitBranchIcon } from "lucide-react";

/** Indents a Branch under its Parent in an Environment tree and marks it ⑂; a root Environment gets nothing. */
export function BranchIndent({ depth }: { depth: number }) {
  if (depth === 0) return null;
  return <>
    <span aria-hidden="true" className="shrink-0" style={{ width: `${depth * 0.75}rem` }} />
    <GitBranchIcon aria-hidden="true" className="shrink-0 text-muted-foreground" />
  </>;
}
