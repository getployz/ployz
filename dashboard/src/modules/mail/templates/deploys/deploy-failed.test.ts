import type { DeploymentView, GitSource } from "@ployz/sdk";
import { expect, it } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { deployFailedMail } from "#/modules/mail/templates/deploys/deploy-failed";

const deployment = (fields: Partial<DeploymentView>) => asTestDouble<DeploymentView>()({
  id: "dep 1", environment: { id: "e", project: "shop", name: "pr 7" }, status: "failed", builds: [], nodes: [], runtime_names: {}, outcome: null, ...fields,
});
const source = (service: string, repository = "acme/web") => asTestDouble<GitSource>()({
  service, repository, branch: "feature/login", commit: "8a7ed6e9f0c1d2b3a4f5e6d7c8b9a0f1e2d3c4b5",
});
const serviceNode = (id: string, name: string, outcome: "failed" | "not_attempted" | "deployed" = "not_attempted") =>
  ({ type: "service" as const, id, name, outcome, rows: [] });
const mail = (fields: Partial<DeploymentView>, sources = [source("web")]) =>
  deployFailedMail({ origin: "https://ployz.dev", organization: { name: "Shop", slug: "shop co" }, deployment: deployment(fields), sources });

it("names the push, the Service and the Environment, and links the failed build's logs", () => {
  const failed = mail({ builds: [{ service: "web", commit: "8a7ed6e", status: "failed", message: "npm ci exited 1" }] });
  expect(failed).toMatchObject({
    subject: "web failed to deploy to shop / pr 7",
    preview: "8a7ed6e on feature/login didn't deploy.",
    title: "web failed to deploy to pr 7",
    paragraphs: ["GitHub started this deploy from feature/login. The logs show what went wrong."],
    action: { label: "View logs", url: "https://ployz.dev/cloud/shop%20co/shop/pr%207/deployments/dep%201?logs=build" },
  });
  expect(failed.facts).toEqual([
    { label: "Repository", value: "acme/web", mono: true },
    { label: "Branch", value: "feature/login", mono: true },
    { label: "Commit", value: "8a7ed6e", mono: true },
    { label: "Environment", value: "shop / pr 7" },
    { label: "Error", value: "npm ci exited 1" },
  ]);
});

it("gives the run's reason over a build's, links deploy logs when no build failed, and says when Ployz lost track", () => {
  const ran = mail({ outcome: { type: "executed", summary: null, reason: "web did not start", cause: ["exit 137"] } });
  expect(ran.facts?.at(-1)).toEqual({ label: "Error", value: "web did not start: exit 137" });
  expect(ran.action?.url).toMatch(/\?logs=deploy$/);
  expect(mail({ status: "unknown" }).facts?.at(-1)?.value).toBe("Ployz lost track of it partway through, so some of it may be live.");
  expect(mail({}).facts?.at(-1)?.value).toBe("It stopped before it said why.");
});

it("describes the Service whose build failed and opens its build log", () => {
  const failed = mail({
    builds: [{ service: "web", commit: "8a7ed6e", status: "built", message: null }, { service: "api", commit: "8a7ed6e", status: "failed", message: "no Dockerfile" }],
    nodes: [serviceNode("svc web", "web"), serviceNode("svc api", "api")],
  }, [source("web"), source("api", "acme/api")]);
  expect(failed.facts?.[0]).toEqual({ label: "Repository", value: "acme/api", mono: true });
  expect(failed.action?.url).toMatch(/\?logs=build&service=svc%20api$/);
});

it("names Services as authored, not by their Private DNS names", () => {
  const failed = mail({
    builds: [{ service: "api", commit: "8a7ed6e", status: "failed", message: "no Dockerfile" }],
    nodes: [serviceNode("svc web", "web"), serviceNode("svc api", "api")],
    runtime_names: { api: "private-api" },
  }, [source("web"), source("private-api", "acme/api")]);
  expect(failed.subject).toBe("api failed to deploy to shop / pr 7");
  expect(failed.facts?.[0]).toEqual({ label: "Repository", value: "acme/api", mono: true });
});

it("names the Service that failed, not the deploy's only Git source", () => {
  const failed = mail({ nodes: [serviceNode("svc web", "web", "deployed"), serviceNode("svc db", "db", "failed")] });
  expect(failed.subject).toBe("db failed to deploy to shop / pr 7");
  expect(mail({ nodes: [serviceNode("svc web", "web"), serviceNode("svc db", "db")] }).subject).toBe("A deploy to shop / pr 7 failed");
  expect(mail({ nodes: [serviceNode("svc web", "web")] }).subject).toBe("web failed to deploy to shop / pr 7");
});

it("counts several failed Services, names no source when no build failed, and falls back to the Environment when nothing names a Service", () => {
  const both = [serviceNode("svc web", "web", "failed"), serviceNode("svc api", "api", "failed")];
  expect(mail({ nodes: both }, [source("web"), source("api")])).toMatchObject({
    subject: "2 services failed to deploy to shop / pr 7",
    paragraphs: ["GitHub started this deploy. The logs show what went wrong."],
    facts: expect.arrayContaining([{ label: "Services", value: "web, api" }]),
  });
  expect(mail({ nodes: both }, [source("web"), source("api")]).facts?.map((fact) => fact.label)).toEqual(["Services", "Environment", "Error"]);
  const bare = mail({}, []);
  expect(bare).toMatchObject({ subject: "A deploy to shop / pr 7 failed", title: "A deploy to pr 7 failed", preview: "The logs show what went wrong." });
  expect(bare.facts?.map((fact) => fact.label)).toEqual(["Environment", "Error"]);
});
