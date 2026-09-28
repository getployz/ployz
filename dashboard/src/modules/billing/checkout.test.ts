import { describe, expect, it, vi } from "vitest";
import { openCheckoutWhileHere } from "#/modules/billing/checkout";

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<T>((res, rej) => { resolve = res; reject = rej; });
  return { promise, resolve, reject };
}

describe("openCheckoutWhileHere", () => {
  it("opens the checkout while the user stays", async () => {
    const checkout = { close: vi.fn() };
    const opened = await openCheckoutWhileHere({ createUrl: async () => "https://polar/a", open: async () => checkout, left: () => false });
    expect(opened).toBe(checkout);
    expect(checkout.close).not.toHaveBeenCalled();
  });

  it("never opens org A's checkout after the user left while it was created", async () => {
    let left = false;
    const url = deferred<string>();
    const open = vi.fn();
    const pending = openCheckoutWhileHere({ createUrl: () => url.promise, open, left: () => left });
    left = true;
    url.resolve("https://polar/org-a");
    expect(await pending).toBeNull();
    expect(open).not.toHaveBeenCalled();
  });

  it("closes a checkout that finished opening after the user left", async () => {
    let left = false;
    const checkout = { close: vi.fn() };
    const opening = deferred<typeof checkout>();
    const open = vi.fn(() => opening.promise);
    const pending = openCheckoutWhileHere({ createUrl: async () => "https://polar/org-a", open, left: () => left });
    await vi.waitFor(() => expect(open).toHaveBeenCalled());
    left = true;
    opening.resolve(checkout);
    expect(await pending).toBeNull();
    expect(checkout.close).toHaveBeenCalledOnce();
  });

  it("drops a failure after the user left, so no error lands on the next page", async () => {
    let left = false;
    const url = deferred<string>();
    const pending = openCheckoutWhileHere({ createUrl: () => url.promise, open: vi.fn(), left: () => left });
    left = true;
    url.reject(new Error("Polar is down"));
    expect(await pending).toBeNull();
  });

  it("still reports a failure while the user stays", async () => {
    await expect(openCheckoutWhileHere({
      createUrl: () => Promise.reject(new Error("Polar is down")), open: vi.fn(), left: () => false,
    })).rejects.toThrow("Polar is down");
  });
});
