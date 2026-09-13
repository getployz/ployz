"use client";

import { useState } from "react";
import { CopyIcon, EyeIcon, EyeOffIcon, InfoIcon } from "lucide-react";
import { Button } from "#/components/ui/button";
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "#/components/ui/tooltip";
import { toast } from "sonner";

export function SecretValueDisplay({
  value = "********",
  maskedValue = "********",
  info = "This value is managed by Ployz.",
}: {
  value?: string;
  maskedValue?: string;
  info?: string;
}) {
  const [revealed, setRevealed] = useState(false);

  async function handleCopy() {
    const clipboard = globalThis.navigator?.clipboard;
    if (!clipboard) {
      toast.error("Failed to copy to clipboard")
      return;
    }

    await clipboard.writeText(value);
    toast.info('Copied to clipboard')
  }

  return (
    <div className="inline-flex items-center gap-1.5">
      <span className="">
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

      <Button
        type="button"
        variant="ghost"
        size="icon-sm"
        onClick={() => {
          void handleCopy();
        }}
      >
        <CopyIcon />
        <span className="sr-only">Copy secret value</span>
      </Button>

      <Tooltip>
        <TooltipTrigger
          render={
            <Button type="button" variant="ghost" size="icon-sm">
              <InfoIcon />
              <span className="sr-only">Secret value information</span>
            </Button>
          }
        />
        <TooltipContent>{info}</TooltipContent>
      </Tooltip>
    </div>
  );
}
