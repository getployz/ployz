import "@tanstack/react-start/server-only";
import { sql, type SQL } from "drizzle-orm";
import { FAILURE_CLASSES, FAILURE_WORDS, type FailureClass, type Phase } from "./uptime-status";

/** The FAILURE_CLASSES of a failing uptime_result row, decided in `failureClass`'s order. */
export const failureClassSql = sql`CASE
  WHEN status <> 0 THEN 'status'
  WHEN error = 'timed_out' THEN 'timeout'
  WHEN error = 'connection_refused' THEN 'refused'
  WHEN failed_phase IN ('dns', 'connect', 'tls') THEN failed_phase
  ELSE 'other' END`;

/** A failing row's error as the tooltip reads it: its word, then the probe's detail. "TLS · invalid peer certificate: Expired". */
export const failureTextSql = (cls: SQL) => sql`(CASE ${cls} WHEN 'status' THEN status::text
  ${sql.join(Object.entries(FAILURE_WORDS).map(([key, word]) => sql`WHEN ${key} THEN ${word}::text`), sql` `)} END
  || CASE WHEN detail <> '' THEN ' · ' || detail ELSE '' END)`;

/** uptime_result_hourly's failure counter for each class. */
export const failureColumn = (cls: FailureClass) => `failures_${cls}` as const;

/** uptime_result_hourly's sum of passing probes' time in each phase. */
export const phaseColumn = (phase: Phase) => `${phase}_ms_sum` as const;

export const failureColumns = sql.raw(FAILURE_CLASSES.map(failureColumn).join(", "));
