// @vitest-environment jsdom
import { act } from "@testing-library/react";
import { createRoot } from "react-dom/client";
import { expect, it, vi } from "vitest";
import { useRestored } from "./use-restored";

function Page({ onRestore }: { onRestore: () => void }) {
  useRestored(onRestore);
  return null;
}

/** Back after Keep restores the page from the back-forward cache, frozen mid-claim, unless it loads again. */
it("acts only when the browser restores the page from the back-forward cache", async () => {
  const onRestore = vi.fn();
  const root = createRoot(document.createElement("div"));
  await act(async () => root.render(<Page onRestore={onRestore} />));
  window.dispatchEvent(new PageTransitionEvent("pageshow", { persisted: false }));
  expect(onRestore).not.toHaveBeenCalled();
  window.dispatchEvent(new PageTransitionEvent("pageshow", { persisted: true }));
  expect(onRestore).toHaveBeenCalledOnce();
  await act(async () => root.unmount());
  window.dispatchEvent(new PageTransitionEvent("pageshow", { persisted: true }));
  expect(onRestore).toHaveBeenCalledOnce();
});
