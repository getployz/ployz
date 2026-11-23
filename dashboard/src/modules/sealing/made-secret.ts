import { parseGenerator } from "@ployz/sdk/config";

/**
 * A value `generator` asks for, made in this browser so Cloud never sees it: `${{ secret(32, "a-zA-Z") }}` gives 32
 * characters drawn uniformly from its charset, as ployz-core's `Generator` reads it.
 */
export function madeSecret(generator: string): string {
  const { length, alphabet } = parseGenerator(generator);
  // Bytes at or past the largest multiple of the alphabet's size are dropped, so every character is equally likely.
  const limit = 256 - (256 % alphabet.length);
  let value = "";
  while (value.length < length) {
    for (const byte of crypto.getRandomValues(new Uint8Array(64))) {
      if (byte < limit && value.length < length) value += alphabet[byte % alphabet.length];
    }
  }
  return value;
}
