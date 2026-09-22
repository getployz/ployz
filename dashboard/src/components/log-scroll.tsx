import { Button } from "./ui/button";
import { useLayoutEffect, useRef, type ReactNode } from "react";
import { useVirtualizer, type VirtualizerOptions } from "@tanstack/react-virtual";

export function useLogScroll(options: Pick<VirtualizerOptions<HTMLDivElement, HTMLDivElement>, "count" | "getItemKey">) {
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
    <div className="flex justify-end"><Button variant="ghost" size="sm" onClick={() => virtual.scrollToEnd()}>Latest</Button></div>
    <div ref={element} className="max-h-80 overflow-auto break-words font-mono text-xs leading-6" tabIndex={0} aria-label="Build logs">
      <div ref={virtual.measureElement} data-index={0}>{children}</div>
    </div>
  </>;
}
