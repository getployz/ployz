"use client";

import { useState } from "react";
import { EyeIcon, EyeOffIcon } from "lucide-react";
import { Button } from "#/components/ui/button";
import { CopyButton } from "#/components/copy-button";

export function SecretValueDisplay({
  value = "********",
  maskedValue = "*******",
}: {
  value?: string;
  maskedValue?: string;
}) {
  const [revealed, setRevealed] = useState(false);

  return (
    <div className="flex min-w-0 flex-1 items-center gap-1.5">
      <span className="min-w-0 flex-1 truncate font-mono text-xs text-muted-foreground">
        {revealed ? value : maskedValue}
      </span>

      <Button
        type="button"
        variant="ghost"
        size="icon-sm"
        onClick={() => setRevealed((current) => !current)}
      >
        {revealed ? <EyeOffIcon /> : <EyeIcon />}
        <span className="sr-only">
          {revealed ? "Hide secret value" : "Show secret value"}
        </span>
      </Button>

      <CopyButton value={value} label="Copy secret value" />

    </div>
  );
}
