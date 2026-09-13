import { useState } from "react";
import { CheckIcon, CopyIcon } from "lucide-react";
import { toast } from "sonner";
import { Button } from "#/components/ui/button";

export function CopyBlock({ value }: { value: string }) {
  const [copied, setCopied] = useState(false);

  async function handleCopy() {
    const clipboard = globalThis.navigator?.clipboard;
    if (!clipboard) {
      toast.error("Couldn't access the clipboard");
      return;
    }
    await clipboard.writeText(value);
    setCopied(true);
    setTimeout(() => setCopied(false), 1500);
  }

  return (
    <div className="relative">
      <pre className="max-h-56 overflow-auto rounded-md border border-border bg-muted p-3 pr-24 font-mono text-xs leading-relaxed text-foreground">
        <code className="whitespace-pre-wrap break-all">{value}</code>
      </pre>
      <Button
        type="button"
        variant="outline"
        size="sm"
        className="absolute top-2 right-2"
        onClick={() => void handleCopy()}
      >
        {copied ? (
          <CheckIcon data-icon="inline-start" />
        ) : (
          <CopyIcon data-icon="inline-start" />
        )}
        {copied ? "Copied" : "Copy"}
      </Button>
    </div>
  );
}
