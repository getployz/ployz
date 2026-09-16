import { useEffect, useRef, useState, type ComponentProps } from "react";
import { CheckIcon, CopyIcon } from "lucide-react";
import { Button } from "#/components/ui/button";
import { copyText } from "#/lib/clipboard";

export function CopyButton({
  value,
  label = "Copy",
  showLabel = false,
  ...props
}: Omit<ComponentProps<typeof Button>, "onClick" | "children"> & {
  value: string;
  label?: string;
  showLabel?: boolean;
}) {
  const [copiedValue, setCopiedValue] = useState<string | null>(null);
  const [copying, setCopying] = useState(false);
  const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  const request = useRef(0);
  useEffect(() => () => {
    request.current += 1;
    clearTimeout(timer.current);
  }, []);
  const copied = copiedValue === value;

  async function handleCopy() {
    const currentRequest = ++request.current;
    setCopying(true);
    clearTimeout(timer.current);
    setCopiedValue(null);
    const succeeded = await copyText(value);
    if (currentRequest !== request.current) return;
    if (succeeded) {
      setCopiedValue(value);
      timer.current = setTimeout(() => setCopiedValue(null), 1500);
    }
    setCopying(false);
  }

  return (
    <Button
      type="button"
      variant="ghost"
      size={showLabel ? "sm" : "icon-sm"}
      {...props}
      disabled={props.disabled || copying}
      aria-label={label}
      onClick={() => void handleCopy()}
    >
      <span className="copy-feedback-icon" data-icon="inline-start" aria-hidden="true">
        <CopyIcon className={copied ? "opacity-0" : "opacity-100"} />
        <CheckIcon className={copied ? "opacity-100" : "opacity-0"} />
      </span>
      {showLabel ? <span className="group/copy-label inline-grid" data-copied={copied}>
        <span className="col-start-1 row-start-1 group-data-[copied=true]/copy-label:invisible">{label}</span>
        <span className="invisible col-start-1 row-start-1 group-data-[copied=true]/copy-label:visible">Copied</span>
      </span> : null}
      <span className="sr-only" role="status">{copied ? "Copied to clipboard" : ""}</span>
    </Button>
  );
}
