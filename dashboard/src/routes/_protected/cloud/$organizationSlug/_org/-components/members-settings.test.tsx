// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { useState } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { InviteDialog, PendingInvites } from "./members-settings";

type Invites = Parameters<typeof PendingInvites>[0]["invites"];

const invite = (id: string, email: string | null): Invites[number] =>
  ({ id, email, role: "member", invited_by: "Ada", expires_at: "2026-10-21T12:00:00Z" });

function Pending({ initial, onRevoke }: { initial: Invites; onRevoke: (invite: string) => void }) {
  const [invites, setInvites] = useState(initial);
  return (
    <PendingInvites invites={invites} manager onInvite={vi.fn()} onRevoke={(id) => {
      onRevoke(id);
      setInvites((current) => current.filter((row) => row.id !== id));
    }} />
  );
}

describe("InviteDialog", () => {
  afterEach(() => {
    cleanup();
    document.body.replaceChildren();
  });

  it.each([
    { mail: true, emailed_to: "grace@example.com", message: "Command says: emailed to grace@example.com." },
    { mail: false, emailed_to: null, message: "Command says: send the link yourself." },
  ])("shows the command's own message after inviting (mail $mail)", async ({ mail, emailed_to, message }) => {
    const invite = vi.fn().mockResolvedValue({
      id: "inv-1",
      email: "grace@example.com",
      role: "member",
      link: "https://cloud.test/invite/t",
      expires_at: "2026-10-21T00:00:00Z",
      emailed_to,
      message: `${message}\nhttps://cloud.test/invite/t`,
    });
    render(
      <QueryClientProvider client={new QueryClient()}>
        <InviteDialog organizationName="Acme" mail={mail} invite={invite} onInvited={vi.fn()} onOpenChange={vi.fn()} />
      </QueryClientProvider>,
    );

    fireEvent.change(screen.getByLabelText("Email (optional)"), { target: { value: "grace@example.com" } });
    fireEvent.click(screen.getByRole("button", { name: "Make link" }));

    expect((await screen.findByLabelText<HTMLInputElement>("Invite link")).value).toBe("https://cloud.test/invite/t");
    expect(screen.getByText(message)).toBeTruthy();
    expect(invite).toHaveBeenCalledWith({ email: "grace@example.com", role: "member" });
  });

  it("moves focus to Copy link once the link is made", async () => {
    const invite = vi.fn().mockResolvedValue({
      id: "inv-1",
      email: null,
      role: "member",
      link: "https://cloud.test/invite/t",
      expires_at: "2026-10-21T00:00:00Z",
      emailed_to: null,
      message: "Send the link yourself.",
    });
    render(
      <QueryClientProvider client={new QueryClient()}>
        <InviteDialog organizationName="Acme" mail={false} invite={invite} onInvited={vi.fn()} onOpenChange={vi.fn()} />
      </QueryClientProvider>,
    );

    fireEvent.click(screen.getByRole("button", { name: "Make link" }));

    const copy = await screen.findByRole("button", { name: "Copy link" });
    await waitFor(() => expect(document.activeElement).toBe(copy));
  });

  it("makes one invite when Make link is double-clicked", async () => {
    const invite = vi.fn().mockResolvedValue({
      id: "inv-1",
      email: "grace@example.com",
      role: "member",
      link: "https://cloud.test/invite/t",
      expires_at: "2026-10-21T00:00:00Z",
      emailed_to: "grace@example.com",
      message: "Emailed to grace@example.com.",
    });
    render(
      <QueryClientProvider client={new QueryClient()}>
        <InviteDialog organizationName="Acme" mail invite={invite} onInvited={vi.fn()} onOpenChange={vi.fn()} />
      </QueryClientProvider>,
    );

    fireEvent.change(screen.getByLabelText("Email (optional)"), { target: { value: "grace@example.com" } });
    const submit = screen.getByRole("button", { name: "Make link" });
    fireEvent.click(submit);
    fireEvent.click(submit);

    await screen.findByLabelText("Invite link");
    expect(invite).toHaveBeenCalledTimes(1);
  });
});

describe("PendingInvites", () => {
  afterEach(() => {
    cleanup();
    document.body.replaceChildren();
  });

  it("names each Revoke after its invite", () => {
    render(<Pending initial={[invite("inv-1", "grace@example.com"), invite("inv-2", null)]} onRevoke={vi.fn()} />);

    expect(screen.getByRole("button", { name: "Revoke grace@example.com" })).toBeTruthy();
    expect(screen.getByRole("button", { name: /^Revoke link expiring / })).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Revoke" })).toBeNull();
  });

  it("moves focus to the next invite after a Revoke, and to Invite after the last", () => {
    const onRevoke = vi.fn();
    render(<Pending initial={[invite("inv-1", "grace@example.com"), invite("inv-2", "linus@example.com")]} onRevoke={onRevoke} />);

    fireEvent.click(screen.getByRole("button", { name: "Revoke grace@example.com" }));
    const next = screen.getByRole("button", { name: "Revoke linus@example.com" });
    expect(document.activeElement).toBe(next);
    expect(screen.queryByRole("button", { name: "Revoke grace@example.com" })).toBeNull();

    fireEvent.click(next);
    expect(document.activeElement).toBe(screen.getByRole("button", { name: "Invite" }));
    expect(onRevoke.mock.calls).toEqual([["inv-1"], ["inv-2"]]);
  });
});
