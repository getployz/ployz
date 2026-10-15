import type { MailContent } from "#/modules/mail/layout";

export type CrashPlace = { readonly project: string; readonly environment: string; readonly serviceId: string };

export type CrashMail = {
  readonly organization: { readonly name: string; readonly slug: string };
  readonly service: string;
  readonly place: CrashPlace | null;
  readonly count: number;
  readonly last: {
    readonly server: { readonly machineId: string; readonly name: string };
    readonly exitCode: number | null;
    readonly oom: boolean;
    readonly at: Date;
  };
  readonly origin: string;
};

const when = (at: Date) =>
  `${at.toLocaleDateString("en-US", { timeZone: "UTC", month: "short", day: "numeric" })}, ${at.toISOString().slice(11, 16)} UTC`;

const ended = ({ oom, exitCode }: CrashMail["last"]) =>
  oom ? "ran out of memory" : exitCode === null ? "exited" : `exited with code ${exitCode}`;

export function crashMail({ organization, service, place, count, last, origin }: CrashMail): Omit<MailContent, "unsubscribe"> {
  const where = place === null ? null : `${place.project} / ${place.environment}`;
  const times = count === 1 ? "crashed" : `crashed ${count} times`;
  const { server } = last;
  const how = ended(last);
  const action = place === null
    ? { label: "View server", url: `${origin}/cloud/${organization.slug}/~/servers/${server.machineId}` }
    : {
        label: "View service",
        url: `${origin}/cloud/${organization.slug}/${encodeURIComponent(place.project)}/${encodeURIComponent(place.environment)}/services/${encodeURIComponent(place.serviceId)}?tab=monitoring`,
      };
  const facts = [
    ...(where === null ? [] : [{ label: "Environment", value: where }]),
    { label: "Server", value: server.name },
    { label: "Crashes", value: String(count) },
    { label: "Last exit", value: last.oom ? "Ran out of memory" : last.exitCode === null ? "Unknown" : `Exit code ${last.exitCode}` },
    { label: "Last crash", value: when(last.at) },
  ];
  return {
    subject: where === null ? `${service} ${times}` : `${service} ${times} in ${where}`,
    preview: `A replica on ${server.name} ${how}.`,
    title: `${service} ${times}`,
    paragraphs: [
      count === 1
        ? `A replica of ${service} on ${server.name} ${how}.`
        : `${service} crashed ${count} times. The last replica to crash was on ${server.name} and ${how}.`,
    ],
    action,
    facts,
    reason: `You're getting this as a member of ${organization.name} on Ployz.`,
  };
}

const WEB_1 = { machineId: "a".repeat(32), name: "web-1" };

const sample = (overrides: Partial<CrashMail>, origin: string): CrashMail => ({
  organization: { name: "Acme", slug: "acme" },
  service: "web",
  place: { project: "shop", environment: "production", serviceId: "c".repeat(32) },
  count: 1,
  last: { server: WEB_1, exitCode: 1, oom: false, at: new Date("2026-10-15T03:02:00Z") },
  origin,
  ...overrides,
});

export const crashSamples = (origin: string) => ({
  "crash.one": crashMail(sample({}, origin)),
  "crash.burst": crashMail(sample({ count: 5, last: { server: WEB_1, exitCode: 137, oom: true, at: new Date("2026-10-15T03:09:00Z") } }, origin)),
  "crash.unplaced": crashMail(sample({ service: "worker", place: null }, origin)),
});
