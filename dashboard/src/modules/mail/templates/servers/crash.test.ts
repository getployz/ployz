import { expect, it } from "vitest";
import { type CrashMail, crashMail } from "#/modules/mail/templates/servers/crash";

const SERVICE = "c".repeat(32);
const MACHINE = "a".repeat(32);
const WEB_1 = { machineId: MACHINE, name: "web-1" };

const mail = (overrides: Partial<CrashMail>) => crashMail({
  organization: { name: "Acme", slug: "acme" },
  service: "web",
  place: { project: "shop", environment: "production", serviceId: SERVICE },
  count: 1,
  last: { server: WEB_1, exitCode: 1, oom: false, at: new Date("2026-10-15T03:02:00Z") },
  origin: "https://ployz.dev",
  ...overrides,
});
const fact = (content: ReturnType<typeof crashMail>, label: string) => content.facts?.find((row) => row.label === label)?.value;

it("names the service, its Environment and the Server, and links the service's monitoring", () => {
  const one = mail({});
  expect(one).toMatchObject({
    subject: "web crashed in shop / production",
    title: "web crashed",
    paragraphs: ["A replica of web on web-1 exited with code 1."],
    action: { label: "View service", url: `https://ployz.dev/cloud/acme/shop/production/services/${SERVICE}?tab=monitoring` },
  });
  expect(fact(one, "Server")).toBe("web-1");
  expect(fact(one, "Last exit")).toBe("Exit code 1");
  expect(fact(one, "Last crash")).toBe("Oct 15, 03:02 UTC");
});

it("counts a burst and says the last crash ran out of memory", () => {
  const burst = mail({ count: 5, last: { server: WEB_1, exitCode: 137, oom: true, at: new Date("2026-10-15T03:09:00Z") } });
  expect(burst).toMatchObject({
    subject: "web crashed 5 times in shop / production",
    paragraphs: ["web crashed 5 times. The last replica to crash was on web-1 and ran out of memory."],
  });
  expect(fact(burst, "Crashes")).toBe("5");
  expect(fact(burst, "Last exit")).toBe("Ran out of memory");
});

it("links the Server when Cloud can't place the service", () => {
  const unplaced = mail({ place: null });
  expect(unplaced).toMatchObject({
    subject: "web crashed",
    action: { label: "View server", url: `https://ployz.dev/cloud/acme/~/servers/${MACHINE}` },
  });
  expect(fact(unplaced, "Environment")).toBeUndefined();
});

it("escapes project and environment names in the link", () => {
  expect(mail({ place: { project: "my shop", environment: "pr/12", serviceId: SERVICE } }).action.url)
    .toBe(`https://ployz.dev/cloud/acme/my%20shop/pr%2F12/services/${SERVICE}?tab=monitoring`);
});
