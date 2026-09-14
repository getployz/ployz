import { toast } from "sonner";

export async function copyText(value: string, successMessage?: string) {
  try {
    if (!globalThis.navigator?.clipboard) {
      toast.error("Couldn't access the clipboard");
      return false;
    }
    await navigator.clipboard.writeText(value);
    if (successMessage) toast.info(successMessage);
    return true;
  } catch {
    toast.error("Couldn't copy to the clipboard. Try again.");
    return false;
  }
}
