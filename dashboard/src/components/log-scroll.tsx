import { Button } from "./ui/button";
import { Skeleton } from "./ui/skeleton";
import { cn } from "#/lib/utils";
import { useLayoutEffect, useRef, type ReactNode } from "react";
import { useVirtualizer, type VirtualizerOptions } from "@tanstack/react-virtual";

export function useLogScroll(options: Pick<VirtualizerOptions<HTMLDivElement, HTMLDivElement>, "count" | "getItemKey" | "onChange" | "paddingStart">) {
  const element = useRef<HTMLDivElement>(null);
  const virtual = useVirtualizer({
    ...options,
    getScrollElement: () => element.current,
    estimateSize: () => 24,
    overscan: 12,
    anchorTo: "end",
    followOnAppend: true,
    scrollEndThreshold: 48,
  });
  useLayoutEffect(() => { virtual.scrollToEnd(); }, [virtual]);
  return { element, virtual };
}

export function BuildLogViewer({ children }: { children: ReactNode }) {
  // Keep arbitrary output chunks in one measured block so split lines stay intact.
  const { element, virtual } = useLogScroll({ count: 1, getItemKey: () => "build-output" });
  return <>
    {!virtual.isAtEnd() ? <div className="flex justify-end"><Button variant="ghost" size="sm" onClick={() => virtual.scrollToEnd()}>Latest</Button></div> : null}
    <div ref={element} className="min-h-0 flex-1 overflow-auto break-words font-mono text-xs leading-6" tabIndex={0} aria-label="Build logs">
      <div ref={virtual.measureElement} data-index={0}>{children}</div>
    </div>
  </>;
}

const LINE_WIDTHS = ["w-2/5", "w-3/5", "w-1/3", "w-1/2"];

/** Log lines' shape (a time, then the message), held until the lines arrive. */
export function LogSkeleton({ rows = LINE_WIDTHS.length, label }: { rows?: number; label: string }) {
  return <ol aria-busy aria-label={label}>
    {LINE_WIDTHS.slice(0, rows).map((width) => <li key={width} className="flex h-6 items-center gap-3 px-1">
      <Skeleton className="h-3 w-16 shrink-0" />
      <Skeleton className={cn("h-3", width)} />
    </li>)}
  </ol>;
}
