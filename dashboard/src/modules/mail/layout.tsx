import { render } from "@react-email/render";
import { Body, Button, Container, Head, Heading, Hr, Html, Img, Link, Preview, Section, Text } from "react-email";

export type MailFact = {
  readonly label: string;
  readonly value: string;
  readonly mono?: boolean;
};

/** One Plain email. The HTML and the plain text both come from these fields, so the two never drift apart. */
export type MailContent = {
  readonly subject: string;
  /** The line inboxes show after the subject. */
  readonly preview: string;
  readonly title: string;
  readonly paragraphs: ReadonlyArray<string>;
  readonly action: { readonly label: string; readonly url: string };
  readonly facts?: ReadonlyArray<MailFact>;
  /** Why this person got the email. */
  readonly reason: string;
  readonly unsubscribe?: { readonly label: string; readonly url: string };
};

export type RenderedMail = {
  readonly subject: string;
  readonly html: string;
  readonly text: string;
};

const SANS = '-apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Helvetica, Arial, sans-serif';
const MONO = 'ui-monospace, SFMono-Regular, Menlo, Consolas, monospace';
const LINE = "1px solid #e6e6e6";

const valueCell = { padding: "7px 0", borderTop: LINE, verticalAlign: "top" };
const monoValueCell = { ...valueCell, fontFamily: MONO, fontSize: "12.5px" };
const footerText = { fontSize: "12px", lineHeight: "1.6", color: "#666666", margin: 0 };
const footerLink = { color: "#666666", textDecoration: "underline" };

function PlainMail({ content, origin }: { content: MailContent; origin: string }) {
  return (
    <Html lang="en">
      <Head />
      <Preview>{content.preview}</Preview>
      <Body style={{ backgroundColor: "#ffffff", color: "#111111", fontFamily: SANS, margin: 0 }}>
        <Container style={{ maxWidth: "520px", margin: 0, padding: "40px 32px" }}>
          <Section>
            <Img
              src={`${origin}/assets/logo-mark.png`}
              width="24"
              height="24"
              alt=""
              style={{ display: "inline-block", verticalAlign: "middle" }}
            />
            <span style={{ fontSize: "15px", fontWeight: 600, letterSpacing: "-0.01em", verticalAlign: "middle", marginLeft: "8px" }}>
              Ployz
            </span>
          </Section>
          <Heading
            as="h1"
            style={{ fontSize: "22px", lineHeight: "1.3", fontWeight: 600, letterSpacing: "-0.015em", margin: "28px 0 12px" }}
          >
            {content.title}
          </Heading>
          {content.paragraphs.map((paragraph) => (
            <Text key={paragraph} style={{ fontSize: "15px", lineHeight: "1.6", color: "#444444", margin: "0 0 16px" }}>
              {paragraph}
            </Text>
          ))}
          <Section style={{ margin: "24px 0 0" }}>
            <Button
              href={content.action.url}
              style={{
                backgroundColor: "#111111",
                color: "#ffffff",
                fontSize: "14px",
                fontWeight: 500,
                padding: "11px 18px",
                borderRadius: "8px",
                textDecoration: "none",
              }}
            >
              {content.action.label}
            </Button>
          </Section>
          {content.facts && content.facts.length > 0 ? (
            <table
              role="presentation"
              width="100%"
              cellPadding={0}
              cellSpacing={0}
              style={{ borderCollapse: "collapse", fontSize: "13px", margin: "24px 0 0" }}
            >
              <tbody>
                {content.facts.map((fact) => (
                  <tr key={fact.label}>
                    <td style={{ width: "110px", padding: "7px 12px 7px 0", borderTop: LINE, color: "#666666", verticalAlign: "top" }}>
                      {fact.label}
                    </td>
                    <td style={fact.mono ? monoValueCell : valueCell}>
                      {fact.value}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          ) : null}
          <Hr style={{ border: 0, borderTop: LINE, margin: "32px 0 16px" }} />
          <Text style={footerText}>
            {content.reason}
            <br />
            {content.unsubscribe ? (
              <>
                <Link href={content.unsubscribe.url} style={footerLink}>
                  {content.unsubscribe.label}
                </Link>
                {" · "}
              </>
            ) : null}
            <Link href={`${origin}/cloud/account`} style={footerLink}>
              Manage emails
            </Link>
          </Text>
        </Container>
      </Body>
    </Html>
  );
}

const plainText = (content: MailContent, origin: string) =>
  [
    content.title,
    "",
    ...content.paragraphs.flatMap((paragraph) => [paragraph, ""]),
    `${content.action.label}: ${content.action.url}`,
    ...(content.facts && content.facts.length > 0 ? ["", ...content.facts.map((fact) => `${fact.label}: ${fact.value}`)] : []),
    "",
    "--",
    content.reason,
    ...(content.unsubscribe ? [`${content.unsubscribe.label}: ${content.unsubscribe.url}`] : []),
    `Manage emails: ${origin}/cloud/account`,
  ].join("\n");

/** `origin` is this Cloud's public URL: the logo and the settings link point at it. */
export const renderMail = async (content: MailContent, origin: string): Promise<RenderedMail> => {
  const base = origin.replace(/\/+$/, "");
  return {
    subject: content.subject,
    html: await render(<PlainMail content={content} origin={base} />),
    text: plainText(content, base),
  };
};
