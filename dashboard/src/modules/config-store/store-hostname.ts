/** Characters no hostname has: controls, whitespace and URL delimiters. */
export const NOT_A_HOSTNAME = /[\p{Cc}\s/\\?#@:%]/u;

/** A hostname as the Store keeps it: IDNA's lowercase punycode, else lowercase where IDNA refuses it. */
export const storedHostname = (hostname: string) => {
  if (NOT_A_HOSTNAME.test(hostname)) return hostname.toLowerCase();
  try {
    return new URL(`http://${hostname}`).hostname;
  } catch {
    return hostname.toLowerCase();
  }
};
