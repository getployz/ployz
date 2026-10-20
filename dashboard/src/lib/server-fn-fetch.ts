import { X_TSS_SERIALIZED } from "@tanstack/react-start";
import type { PublicError } from "#/lib/public-error";

const FAILED: PublicError = { _tag: "PublicError", code: "UNAVAILABLE", message: "Ployz Cloud had a problem. Try again." };

/**
 * Every server function call from the browser. Cloud serializes its own answers, failures included; an unserialized
 * error came from in front of it (a proxy, a gateway), and TanStack would hand its body on as the answer.
 */
export async function serverFnFetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response> {
  const response = await fetch(input, init);
  if (response.status >= 500 && !response.headers.has(X_TSS_SERIALIZED)) throw FAILED;
  return response;
}
