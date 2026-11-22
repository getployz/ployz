/** Where Ployz's public site and docs live, and the Cloud that self-hosted installs default to. */
export const PLOYZ_ORIGIN = "https://ployz.dev";

/** A page of the public docs or site, from its path (`/docs/self-hosting#2-configure`). */
export const docsUrl = (path: string) => `${PLOYZ_ORIGIN}${path}`;
