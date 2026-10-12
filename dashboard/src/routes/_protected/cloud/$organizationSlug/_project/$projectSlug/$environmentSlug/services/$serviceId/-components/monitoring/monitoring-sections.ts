/**
 * What a Service's Monitoring tab holds. Uptime needs a Cloud that runs the uptime checker. Ployz collects no Service
 * metrics yet, so dev builds draw invented ones to judge the layout. No section, no tab.
 */
export type MonitoringSection = "uptime" | "metrics";

export function monitoringSections({ uptimeEnabled, metrics = import.meta.env.DEV }: { uptimeEnabled: boolean; metrics?: boolean }): readonly MonitoringSection[] {
  return [...(uptimeEnabled ? ["uptime" as const] : []), ...(metrics ? ["metrics" as const] : [])];
}
