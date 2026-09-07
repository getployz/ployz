// @vitest-environment jsdom

import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import { Card } from "#/components/ui/card";
import { Input } from "#/components/ui/input";
import {
  InputGroup,
  InputGroupInput,
} from "#/components/ui/input-group";
import { Item } from "#/components/ui/item";
import {
  Table,
  TableBody,
  TableCell,
  TableRow,
} from "#/components/ui/table";
import { Textarea } from "#/components/ui/textarea";

describe("semantic UI states", () => {
  it("applies primary styling hooks to changed inputs and textareas", () => {
    render(
      <>
        <Input aria-label="Service name" data-changed="true" />
        <Textarea aria-label="Commit message" data-changed="true" />
        <Button data-changed="true">Save changes</Button>
        <InputGroup data-changed="true">
          <InputGroupInput aria-label="Repository" />
        </InputGroup>
      </>,
    );

    const input = screen.getByLabelText("Service name");
    const textarea = screen.getByLabelText("Commit message");
    const button = screen.getByRole("button", { name: "Save changes" });
    const inputGroup = screen
      .getByLabelText("Repository")
      .closest('[data-slot="input-group"]');

    expect(input.getAttribute("data-changed")).toBe("true");
    expect(input.className).toContain("data-[changed=true]:bg-changed-soft");
    expect(textarea.getAttribute("data-changed")).toBe("true");
    expect(textarea.className).toContain(
      "data-[changed=true]:bg-changed-soft",
    );
    expect(button.className).toContain("data-[changed=true]:bg-changed-soft");
    expect(inputGroup?.getAttribute("data-changed")).toBe("true");
    expect(inputGroup?.className).toContain(
      "data-[changed=true]:bg-changed-soft",
    );
  });

  it("applies semantic classes to info and changed surfaces", () => {
    render(
      <>
        <Badge variant="success">Success</Badge>
        <Badge variant="warning">Warning</Badge>
        <Badge variant="info">Info</Badge>
        <Badge variant="changed">Changed</Badge>
        <Item state="info">Info row</Item>
        <Item state="changed">Changed row</Item>
        <Card state="info">Info card</Card>
        <Card state="changed">Changed card</Card>
        <Card state="success">Created card</Card>
      </>,
    );

    expect(screen.getByText("Success").className).toContain(
      "bg-success-soft",
    );
    expect(screen.getByText("Warning").className).toContain(
      "bg-warning-soft",
    );
    expect(screen.getByText("Info").className).toContain(
      "bg-info-soft",
    );
    expect(screen.getByText("Changed").className).toContain(
      "bg-changed-soft",
    );
    expect(screen.getByText("Info row").className).toContain(
      "bg-info-soft",
    );
    expect(screen.getByText("Changed row").className).toContain(
      "bg-changed-soft",
    );

    const infoCard = screen.getByText("Info card").closest('[data-slot="card"]');
    const changedCard = screen
      .getByText("Changed card")
      .closest('[data-slot="card"]');
    const card = screen.getByText("Created card").closest('[data-slot="card"]');

    expect(infoCard?.className).toContain("bg-info-soft");
    expect(changedCard?.className).toContain("bg-changed-soft");

    expect(card?.getAttribute("data-state")).toBe("success");
    expect(card?.className).toContain("bg-success-soft");
  });

  it("keeps the compact node card layout contract", () => {
    render(<Card size="node">Node card</Card>);

    const card = screen.getByText("Node card");

    expect(card.getAttribute("data-size")).toBe("node");
    expect(card.className).toContain("data-[size=node]:gap-0");
    expect(card.className).toContain("data-[size=node]:text-xs");
  });

  it("applies semantic row styling for destructive changes", () => {
    render(
      <Table>
        <TableBody>
          <TableRow state="destructive">
            <TableCell>Remove</TableCell>
          </TableRow>
        </TableBody>
      </Table>,
    );

    const row = screen.getByText("Remove").closest('[data-slot="table-row"]');

    expect(row?.getAttribute("data-semantic-state")).toBe("destructive");
    expect(row?.className).toContain("bg-destructive-soft");
  });
});
