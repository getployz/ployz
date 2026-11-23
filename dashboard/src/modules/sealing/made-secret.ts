/**
 * A value `generator` asks for, made in this browser so Cloud never sees it: `${{ secret(32, "a-zA-Z") }}` gives 32
 * characters drawn uniformly from its charset, as ployz-core's `Generator` reads it.
 */
export function madeSecret(generator: string): string {
  const call = /^\$\{\{\s*secret\(\s*(\d+)\s*,\s*"([^"\\']+)"\s*\)\s*\}\}$/u.exec(generator);
  const alphabet = call ? expand(call[2] ?? "") : [];
  const length = Number(call?.[1]);
  if (alphabet.length < 2 || !Number.isInteger(length) || length < 8 || length > 256) {
    throw new Error("This browser can't make that kind of value.");
  }
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

/** ployz-core's `expand`: `a-z` is a range, anything else one character; sorted, each once. */
function expand(charset: string) {
  const chars = new Set<string>();
  for (let at = 0; at < charset.length;) {
    const first = charset.charCodeAt(at);
    const last = charset[at + 1] === "-" && at + 2 < charset.length ? charset.charCodeAt(at + 2) : null;
    if (last === null) {
      chars.add(charset[at] ?? "");
      at += 1;
      continue;
    }
    if (first > last) return [];
    for (let code = first; code <= last; code++) chars.add(String.fromCharCode(code));
    at += 3;
  }
  return [...chars].sort();
}
