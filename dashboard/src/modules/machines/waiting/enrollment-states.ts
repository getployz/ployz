export type EnrollmentRole = "founder" | "joiner";

export type EnrollmentState = "awaiting_founder" | "instructed" | "running" | "joined" | "failed" | "cancelled";

export type EnrollmentStage = "installing" | "storage" | "enrolling" | "queued" | "joining" | "publishing";

export type EnrollStorage = { readonly kind: "none" } | { readonly kind: "zfs"; readonly device?: string };

export type EnrollPolicy = {
  readonly labels: Readonly<Record<string, string>>;
  readonly acceptsBuilds: boolean;
  readonly acceptsServices: boolean;
  readonly acceptsIngress: boolean;
};

/** What the host runs once Cloud answers `enroll`; the token travels beside it, never inside it. */
export type EnrollInstruction = {
  readonly name: string;
  readonly storage: EnrollStorage;
  readonly policy: EnrollPolicy;
  /** The founder's only: the Cluster Name it founds with, its `--cluster-name`. */
  readonly clusterName?: string;
  /** The founder's only: found with every Door closed, its `--ingress-closed`. */
  readonly ingressClosed?: true;
};

export type EnrollmentFailure = { readonly stage: EnrollmentStage | null; readonly message: string };
