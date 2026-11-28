// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { tolerateForeignDom } from "#/lib/foreign-dom";
import { asTestDouble } from "#/lib/test-double";
import type { ClusterNameControl } from "#/modules/cluster-name/cluster-name.hooks";
import * as doorsFunctions from "#/modules/ingress/doors.functions";
import * as enrollmentFunctions from "#/modules/machines/enrollment.functions";
import * as memberFunctions from "#/modules/organization/members.functions";
import { membersKey, ROLE_CHECKING, ROLE_UNCHECKED } from "#/modules/organization/members.queries";
import { forgetCloudflare, readHeldCloudflare } from "#/modules/ingress/cloudflare-setup";
import { FirstRunCard } from "./first-run-card";

const rename = vi.fn();
const cluster = asTestDouble<ClusterNameControl>()({ view: { state: "current", name: "ada" }, rename });

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  rename.mockReset();
  forgetCloudflare("ada");
});

const owner = () => {
  const client = new QueryClient();
  client.setQueryData(membersKey("ada"), { members: [{ you: true, role: "owner", name: "Ada", email: "ada@example.com", joined_at: "2026-10-01T00:00:00Z" }], invites: [], mail: false, support: false });
  return client;
};

const line = (ingressClosed = false) => ({
  token: "pmet_first", expiresAt: "2026-10-24T10:00:00Z", ingressClosed, cloudOrigin: "https://ployz.dev", ployzVersion: "1.2.3",
});
const minted = () => vi.spyOn(enrollmentFunctions, "mintMachineEnrollmentServerFn").mockImplementation(({ data }) => Promise.resolve(line(data.ingressClosed)));
const SHOWN_ONCE = "Shown once. Works for one server, for 24 hours.";

it("offers the install command only once your role allows it", async () => {
  let resolve!: (members: Awaited<ReturnType<typeof memberFunctions.listMembersServerFn>>) => void;
  vi.spyOn(memberFunctions, "listMembersServerFn").mockReturnValue(new Promise((settle) => { resolve = settle; }));
  render(<QueryClientProvider client={new QueryClient()}><FirstRunCard organizationSlug="ada" cluster={cluster} /></QueryClientProvider>);
  const get = screen.getByRole("button", { name: "Get install command" });
  expect(get).toHaveProperty("disabled", true);
  expect(get.getAttribute("title")).toBe(ROLE_CHECKING);
  expect(screen.queryByText("Only an owner or admin can add servers.")).toBeNull();
  await act(async () => resolve({ members: [{ you: true, role: "owner", name: "Ada", email: "ada@example.com", joined_at: "2026-10-01T00:00:00Z" }], invites: [], mail: false, support: false }));
  await waitFor(() => expect(get).toHaveProperty("disabled", false));
});

it("keeps the install command closed when your role can't be read", async () => {
  vi.spyOn(memberFunctions, "listMembersServerFn").mockRejectedValue(new Error("offline"));
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(<QueryClientProvider client={client}><FirstRunCard organizationSlug="ada" cluster={cluster} /></QueryClientProvider>);
  await waitFor(() => expect(client.getQueryState(membersKey("ada"))?.status).toBe("error"));
  expect(screen.getByRole("button", { name: "Get install command" })).toHaveProperty("disabled", true);
  expect(screen.getByRole("button", { name: "Get install command" }).getAttribute("title")).toBe(ROLE_UNCHECKED);
  expect(screen.queryByText("Only an owner or admin can add servers.")).toBeNull();
});

it("offers the install command on a support visit, as Cloud does", () => {
  const client = new QueryClient();
  client.setQueryData(membersKey("ada"), { members: [], invites: [], mail: false, support: true });
  render(<QueryClientProvider client={client}><FirstRunCard organizationSlug="ada" cluster={cluster} /></QueryClientProvider>);
  expect(screen.getByRole("button", { name: "Get install command" })).toHaveProperty("disabled", false);
});

it("makes one Server's token, as ployz server add --command does", async () => {
  const create = minted();
  const client = new QueryClient();
  client.setQueryData(membersKey("ada"), { members: [], invites: [], mail: false, support: true });
  render(<QueryClientProvider client={client}><FirstRunCard organizationSlug="ada" cluster={cluster} /></QueryClientProvider>);
  fireEvent.click(screen.getByRole("button", { name: "Get install command" }));
  await screen.findByText(SHOWN_ONCE);
  expect(create).toHaveBeenCalledWith({ data: { organizationSlug: "ada", ingressClosed: false } });
});

/** Does to the page what Chrome's translate does: each text node is swapped for `<font><font>translated</font></font>`. */
function translate(root: HTMLElement) {
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
  const texts: Text[] = [];
  while (walker.nextNode()) texts.push(walker.currentNode as Text);
  for (const text of texts) {
    const outer = document.createElement("font");
    const inner = outer.appendChild(document.createElement("font"));
    inner.textContent = `es: ${text.data}`;
    text.replaceWith(outer);
  }
}

it("gets the install command on a page the browser translated", async () => {
  tolerateForeignDom();
  let answer!: () => void;
  vi.spyOn(enrollmentFunctions, "mintMachineEnrollmentServerFn").mockReturnValue(new Promise((settle) => { answer = () => settle(line()); }));
  const { container } = render(<QueryClientProvider client={owner()}><FirstRunCard organizationSlug="ada" cluster={cluster} /></QueryClientProvider>);
  translate(container);
  const get = screen.getByRole("button", { name: "es: Get install command" });
  fireEvent.click(get);
  await waitFor(() => expect(get.querySelector('[data-slot="spinner"]')).not.toBeNull());
  await act(async () => answer());
  expect(await screen.findByText(SHOWN_ONCE)).toBeTruthy();
});

it("keeps the Cloudflare choice collapsed under the install command, and a line with it untouched founds with public ports", async () => {
  const create = minted();
  render(<QueryClientProvider client={owner()}><FirstRunCard organizationSlug="ada" cluster={cluster} /></QueryClientProvider>);
  const disclosure = screen.getByRole("button", { name: "Use Cloudflare Tunnel instead of public ports" });
  expect(screen.queryByLabelText("Cluster name")).toBeNull();
  fireEvent.click(disclosure);
  expect(screen.getByLabelText<HTMLInputElement>("Cluster name").value).toBe("ada");
  fireEvent.click(disclosure);
  fireEvent.click(screen.getByRole("button", { name: "Get install command" }));
  await screen.findByText(SHOWN_ONCE);
  expect(create).toHaveBeenCalledWith({ data: expect.objectContaining({ ingressClosed: false }) });
  expect(rename).not.toHaveBeenCalled();
  expect(readHeldCloudflare("ada")).toBeUndefined();
});

it("waits on Cloudflare before the install command, and falls back to public ports and says so when it's left half way", async () => {
  vi.spyOn(doorsFunctions, "listCloudflareZonesServerFn").mockResolvedValue({ zones: ["zenploy.dev"] });
  render(<QueryClientProvider client={owner()}><FirstRunCard organizationSlug="ada" cluster={cluster} /></QueryClientProvider>);
  fireEvent.click(screen.getByRole("button", { name: "Use Cloudflare Tunnel instead of public ports" }));
  fireEvent.click(screen.getByRole("switch", { name: "Cloudflare Tunnel" }));
  expect(screen.getByRole("button", { name: "Get install command" })).toHaveProperty("disabled", true);
  fireEvent.change(screen.getByLabelText("Cloudflare API token"), { target: { value: "cf-token" } });
  fireEvent.click(screen.getByRole("button", { name: "Find zones" }));
  await screen.findByLabelText("Zone");
  fireEvent.click(screen.getByRole("button", { name: "Use Cloudflare Tunnel instead of public ports" }));
  expect(screen.getByRole("button", { name: "Get install command" })).toHaveProperty("disabled", false);
  expect(screen.getByRole("status").textContent).toContain("Without Cloudflare Tunnel, your servers open ports 80 and 443.");
});

it("founds closed once Cloudflare checks out, holding the token in the tab only after the line exists, with the edited name saved", async () => {
  vi.spyOn(doorsFunctions, "listCloudflareZonesServerFn").mockResolvedValue({ zones: ["zenploy.dev"] });
  const check = vi.spyOn(doorsFunctions, "checkCloudflareServerFn").mockResolvedValue({ zone: "zenploy.dev" });
  const create = minted();
  render(<QueryClientProvider client={owner()}><FirstRunCard organizationSlug="ada" cluster={cluster} /></QueryClientProvider>);
  fireEvent.click(screen.getByRole("button", { name: "Use Cloudflare Tunnel instead of public ports" }));
  fireEvent.change(screen.getByLabelText("Cluster name"), { target: { value: "acme" } });
  fireEvent.click(screen.getByRole("switch", { name: "Cloudflare Tunnel" }));
  fireEvent.change(screen.getByLabelText("Cloudflare API token"), { target: { value: "cf-token" } });
  fireEvent.click(screen.getByRole("button", { name: "Find zones" }));
  await screen.findByLabelText("Zone");
  fireEvent.click(screen.getByRole("button", { name: "Check" }));
  await screen.findByText(/Ready for/u);
  expect(check).toHaveBeenCalledWith({ data: { organizationSlug: "ada", token: "cf-token", zone: "zenploy.dev" } });
  expect(readHeldCloudflare("ada")).toBeUndefined();
  fireEvent.click(screen.getByRole("button", { name: "Get install command" }));
  await screen.findByText(/Keep this tab open until your server joins/u);
  expect(create).toHaveBeenCalledWith({ data: expect.objectContaining({ ingressClosed: true }) });
  expect(rename).toHaveBeenCalledWith("acme");
  expect(readHeldCloudflare("ada")).toEqual({ kind: "waiting", token: "cf-token", zone: "zenploy.dev" });
});
