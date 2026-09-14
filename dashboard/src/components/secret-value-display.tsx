"use client";

import { useState } from "react";
import { EyeIcon, EyeOffIcon, InfoIcon } from "lucide-react";
import { Button } from "#/components/ui/button";
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "#/components/ui/tooltip";
import { CopyButton } from "#/components/copy-button";

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

      <CopyButton value={value} label="Copy secret value" />

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
