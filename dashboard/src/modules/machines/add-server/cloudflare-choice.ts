/** The first-run card's Cloudflare Tunnel choice, one step at a time. `off` remembers a setup left half way. */
export type CardCloudflare =
  | { readonly at: "off"; readonly abandoned: boolean }
  | { readonly at: "token" }
  | { readonly at: "zone"; readonly zones: readonly string[] }
  | { readonly at: "ready"; readonly zone: string };

/** Whether Get install command may run for the card's choice, what the card says about it, and whether the line founds closed. */
export type InstallGate = { readonly allowed: boolean; readonly note: string | null; readonly foundsClosed: boolean };

export function installGate(choice: CardCloudflare): InstallGate {
  switch (choice.at) {
    case "off":
      return { allowed: true, note: choice.abandoned ? "Without Cloudflare Tunnel, your servers open ports 80 and 443." : null, foundsClosed: false };
    case "token":
    case "zone":
      return { allowed: false, note: null, foundsClosed: false };
    case "ready":
      return { allowed: true, note: null, foundsClosed: true };
  }
}

/** Turning Cloudflare off, or closing the section mid-setup, falls back to public ports and says so. */
export const abandonCloudflare = (choice: CardCloudflare): CardCloudflare =>
  choice.at === "off" ? choice : { at: "off", abandoned: true };
