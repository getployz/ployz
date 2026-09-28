/**
 * Opens a checkout unless the user left (another organization, another page) while it loaded.
 * A checkout that opens after they left is closed, and a failure after they left is dropped, so neither lands on
 * the page they went to. Resolves `null` when they left.
 */
export async function openCheckoutWhileHere<Checkout extends { close(): void }>({ createUrl, open, left }: {
  createUrl: () => Promise<string>;
  open: (url: string) => Promise<Checkout>;
  left: () => boolean;
}): Promise<Checkout | null> {
  try {
    const url = await createUrl();
    if (left()) return null;
    const checkout = await open(url);
    if (!left()) return checkout;
    checkout.close();
    return null;
  } catch (error) {
    if (left()) return null;
    throw error;
  }
}
