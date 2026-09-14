import { CopyButton } from "#/components/copy-button";

export function CopyBlock({ value }: { value: string }) {
  return (
    <div className="relative">
      <pre className="max-h-56 overflow-auto rounded-md border border-border bg-muted p-3 pr-24 font-mono text-xs leading-relaxed text-foreground">
        <code className="whitespace-pre-wrap break-all">{value}</code>
      </pre>
      <CopyButton value={value} showLabel variant="outline" className="absolute top-2 right-2" />
    </div>
  );
}
