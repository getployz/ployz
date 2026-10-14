import { Data } from "effect";

/**
 * Who a repository read is for: a public repository needs no credentials; an Organization reads through an installation
 * it links; Cloud itself reads as the installation only to act on a webhook for every Organization linked to it. A
 * repository request without one of these does not compile.
 */
export type GithubAccess =
  | { readonly type: "public" }
  | { readonly type: "organization"; readonly organizationId: string; readonly installationId: number }
  | { readonly type: "installation"; readonly installationId: number };

/** The Organization no longer links the installation, so no token is minted or reused for it. */
export class GithubDisconnected extends Data.TaggedError("GithubDisconnected")<{
  readonly organizationId: string;
  readonly installationId: number;
  readonly accountLogin: string;
}> {
  readonly publicErrorCategory = "forbidden" as const;
  readonly retriable = false as const;
  override get message() {
    return `GitHub account \`${this.accountLogin}\` was disconnected`;
  }
}

export const organizationAccess = (organizationId: string, installationId: number): GithubAccess =>
  ({ type: "organization", organizationId, installationId });

export const installationAccess = (installationId: number): GithubAccess => ({ type: "installation", installationId });

export const accessFor = (organizationId: string, installationId: number | null): GithubAccess =>
  installationId === null ? { type: "public" } : organizationAccess(organizationId, installationId);
