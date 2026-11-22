import type { ReactNode } from "react";

/** One thing a deletion takes with it, drawn with its canvas icon. */
export type DeletionItem = {
  kind: "service" | "volume" | "branch" | "environment" | "project" | "server";
  name: string;
  /** Replaces the kind's icon, such as a service's source icon. */
  icon?: ReactNode;
  /** A volume's used bytes, when its server reported them. */
  bytes?: number;
  /** Muted text after the name, such as the server a volume is on. */
  detail?: string;
  /** Changes after this are lost: a restore point, dated when it isn't from today, as the clock moves. */
  lostAfter?: Date;
};

/** What the servers reported, and the evidence confirming exactly that takes. */
export type DeletionCheck<Evidence> = { items: readonly DeletionItem[]; evidence: Evidence };
