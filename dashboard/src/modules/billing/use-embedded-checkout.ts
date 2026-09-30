import { useCallback, useEffect, useRef, useState } from "react";
import { useServerFn } from "@tanstack/react-start";
import { toast } from "sonner";
import { createEmbeddedCheckoutServerFn } from "#/modules/billing/billing.functions";
import { openCheckoutWhileHere } from "#/modules/billing/checkout";

/**
 * Polar's embedded checkout for an Organization, over the current page. With `onSuccess` the buyer stays where they
 * are instead of following Polar's redirect to the success URL. Leaving the Organization closes it.
 */
export function useEmbeddedCheckout(organizationSlug: string, onSuccess?: () => void) {
  const [pending, setPending] = useState(false);
  const createEmbeddedCheckout = useServerFn(createEmbeddedCheckoutServerFn);
  const activeCheckoutRef = useRef<{ close(): void } | null>(null);
  // Identifies this organization's visit; a checkout that resolves after it ends must not open.
  const visitRef = useRef<object | null>(null);
  const onSuccessRef = useRef(onSuccess);
  onSuccessRef.current = onSuccess;

  const closeActiveCheckout = useCallback(() => {
    const activeCheckout = activeCheckoutRef.current;
    activeCheckoutRef.current = null;
    activeCheckout?.close();
  }, []);

  useEffect(() => {
    visitRef.current = {};
    return () => {
      visitRef.current = null;
      closeActiveCheckout();
    };
  }, [organizationSlug, closeActiveCheckout]);

  async function openCheckout() {
    const visit = visitRef.current;
    try {
      setPending(true);
      const activeCheckout = await openCheckoutWhileHere({
        createUrl: async () => (await createEmbeddedCheckout({ data: { organizationSlug } })).url,
        open: async (url) => {
          closeActiveCheckout();
          const { PolarEmbedCheckout } = await import("@polar-sh/checkout/embed");
          return PolarEmbedCheckout.create(url, { theme: "light" });
        },
        left: () => visitRef.current !== visit,
      });
      if (!activeCheckout) return;

      activeCheckout.addEventListener("close", () => {
        if (activeCheckoutRef.current === activeCheckout) {
          activeCheckoutRef.current = null;
        }
      });
      activeCheckout.addEventListener("success", (event) => {
        const stay = onSuccessRef.current;
        if (!stay) return;
        event.preventDefault();
        closeActiveCheckout();
        stay();
      });
      activeCheckoutRef.current = activeCheckout;
    } catch {
      toast.error("Unable to start checkout.");
    } finally {
      setPending(false);
    }
  }

  return { openCheckout, pending };
}
