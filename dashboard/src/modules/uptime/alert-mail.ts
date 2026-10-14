import { STATUS_CODES } from "node:http";
import type { MailContent } from "#/modules/mail/layout";
import { failureClass, incidentRange } from "./uptime";
import { instantEdge } from "./uptime-range";
import type { FailureClass, IncidentObservation } from "./uptime-status";

/** What an uptime email tells, for an incident that opened or one that closed. */
export type UptimeAlert = {
  readonly organization: { readonly slug: string; readonly name: string };
  readonly project: string;
  readonly environment: string;
  readonly service: { readonly id: string; readonly name: string };
  readonly checkUrl: string;
  readonly incidentId: string;
  readonly observations: readonly IncidentObservation[];
  readonly openedAt: Date;
} & ({ readonly kind: "opened" } | { readonly kind: "resolved"; readonly closedAt: Date });

const FAILURE_PHRASES = {
  timeout: "Timed out",
  refused: "Connection refused",
  dns: "DNS lookup failed",
  connect: "Could not connect",
  tls: "TLS handshake failed",
  other: "Request failed",
} as const satisfies Record<Exclude<FailureClass, "status">, string>;

function failedWith(observations: readonly IncidentObservation[]): string {
  const failure = observations.find((observation) => observation.verdict === "fail");
  if (failure === undefined) return "Unknown";
  const cls = failureClass(failure);
  return cls === "status" ? `${failure.status} ${STATUS_CODES[failure.status] ?? ""}`.trim() : FAILURE_PHRASES[cls];
}

const clock = (at: Date) => at.toISOString().slice(11, 16);
const day = (at: Date) => at.toLocaleDateString("en-US", { timeZone: "UTC", month: "short", day: "numeric" });

function downSpan(from: Date, to: Date): string {
  const sameDay = from.toISOString().slice(0, 10) === to.toISOString().slice(0, 10);
  return sameDay
    ? `${clock(from)} to ${clock(to)} UTC`
    : `${day(from)} ${clock(from)} to ${day(to)} ${clock(to)} UTC`;
}

function duration(ms: number): string {
  const minutes = Math.round(ms / 60_000);
  if (minutes < 1) return "less than a minute";
  if (minutes < 60) return minutes === 1 ? "1 minute" : `${minutes} minutes`;
  const hours = Math.floor(minutes / 60);
  const rest = minutes % 60;
  const hourText = hours === 1 ? "1 hour" : `${hours} hours`;
  return rest === 0 ? hourText : `${hourText} ${rest === 1 ? "1 minute" : `${rest} minutes`}`;
}

/** The service's Monitoring tab, zoomed on the incident as the tab zooms one, and left open-ended while it lasts. */
function incidentLink(origin: string, alert: UptimeAlert, now: Date): string {
  const base = `${origin}/cloud/${alert.organization.slug}/${encodeURIComponent(alert.project)}/${encodeURIComponent(alert.environment)}`;
  const search = new URLSearchParams({ tab: "monitoring" });
  if (alert.kind === "opened") {
    search.set("from", instantEdge(alert.openedAt.getTime() - 3_600_000));
  } else {
    const entry = { id: alert.incidentId, openedAt: alert.openedAt.toISOString(), closedAt: alert.closedAt.toISOString(), observations: alert.observations };
    const range = incidentRange(entry, Math.max(now.getTime(), alert.closedAt.getTime()));
    search.set("from", instantEdge(range.from));
    search.set("to", instantEdge(range.to));
  }
  return `${base}/services/${encodeURIComponent(alert.service.id)}?${search.toString()}`;
}

/** The down or recovered email for `alert`, without its unsubscribe link, which each recipient gets their own of. */
export function uptimeAlertMail(alert: UptimeAlert, origin: string, now: Date): Omit<MailContent, "unsubscribe"> {
  const name = alert.service.name;
  const where = `${alert.project} / ${alert.environment}`;
  const facts = [
    { label: "Service", value: name },
    { label: "Environment", value: where },
    { label: "Check", value: `GET ${alert.checkUrl}`, mono: true },
  ];
  const shared = {
    action: { label: "View incident", url: incidentLink(origin, alert, now) },
    reason: `You're getting this because you're a member of ${alert.organization.name} on Ployz.`,
  };
  if (alert.kind === "opened") {
    const since = `${clock(alert.openedAt)} UTC`;
    return {
      ...shared,
      subject: `Down: ${name} in ${where}`,
      preview: `The health check fails since ${since}.`,
      title: `${name} is down`,
      paragraphs: [`The health check on ${name} fails since ${since}. We'll email again when it recovers.`],
      facts: [...facts, { label: "Failed with", value: failedWith(alert.observations) }],
    };
  }
  const down = duration(alert.closedAt.getTime() - alert.openedAt.getTime());
  return {
    ...shared,
    subject: `Recovered: ${name} in ${where}`,
    preview: `Back up after ${down}.`,
    title: `${name} is back up`,
    paragraphs: [`The health check passes again. ${name} was down for ${down}.`],
    facts: [...facts, { label: "Down", value: downSpan(alert.openedAt, alert.closedAt) }],
  };
}

const sampleAlert = {
  organization: { slug: "acme", name: "Acme" },
  project: "shop",
  environment: "production",
  service: { id: "00000000-0000-4000-8000-0000000000a1", name: "api" },
  checkUrl: "https://api.acme.com/health",
  incidentId: "00000000-0000-4000-8000-0000000000b1",
  observations: [
    { region: "fra", kind: "confirm", verdict: "fail", status: 502, failedPhase: null, error: null, remoteIp: null, at: "2026-10-14T03:02:00.000Z" },
  ],
  openedAt: new Date("2026-10-14T03:02:00.000Z"),
} as const;

/** Both uptime emails for a made-up incident on Acme's api, for previews and test sends. */
export const uptimeAlertSamples = (origin: string) => {
  const now = new Date("2026-10-14T03:20:00.000Z");
  return {
    "uptime-down": uptimeAlertMail({ ...sampleAlert, kind: "opened" }, origin, now),
    "uptime-recovered": uptimeAlertMail({ ...sampleAlert, kind: "resolved", closedAt: new Date("2026-10-14T03:14:00.000Z") }, origin, now),
  };
};
