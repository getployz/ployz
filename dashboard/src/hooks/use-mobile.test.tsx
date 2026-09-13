// @vitest-environment jsdom

import { render } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { useIsMobile } from "#/hooks/use-mobile";

function setViewportWidth(width: number) {
  Object.defineProperty(window, "innerWidth", {
    configurable: true,
    value: width,
  });
}

function setMatchMedia() {
  const listeners = new Set<EventListener>();
  const matchMedia = vi.fn((query: string) => {
    const mediaQueryList = {
      matches: window.innerWidth <= 860,
      media: query,
      onchange: null,
      addEventListener: (
        _event: string,
        listener: EventListenerOrEventListenerObject,
      ) => {
        if (listener instanceof Function) {
          listeners.add(listener);
        }
      },
      removeEventListener: (
        _event: string,
        listener: EventListenerOrEventListenerObject,
      ) => {
        if (listener instanceof Function) {
          listeners.delete(listener);
        }
      },
      addListener: vi.fn(),
      removeListener: vi.fn(),
      dispatchEvent: vi.fn(),
    } as MediaQueryList;

    return mediaQueryList;
  });

  Object.defineProperty(window, "matchMedia", {
    configurable: true,
    value: matchMedia,
  });

  return { listeners, matchMedia };
}

function MobileProbe() {
  return <div>{useIsMobile() ? "mobile" : "desktop"}</div>;
}

describe("useIsMobile", () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("returns the current viewport state on the first client render", () => {
    setViewportWidth(500);
    setMatchMedia();

    const { container } = render(<MobileProbe />);

    expect(container.textContent).toBe("mobile");
  });

  it("uses the shared 860px navigation breakpoint", () => {
    setViewportWidth(860);
    setMatchMedia();

    const { container } = render(<MobileProbe />);

    expect(container.textContent).toBe("mobile");
  });
});
