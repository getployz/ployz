# Writing the Ployz docs

Write like Railway on a good day: a calm tour of what Ployz does for you and how to do it.
Never like Coolify: no manual of how Ployz works inside.

**The test for every sentence: does it help the reader do the thing, understand how the
pieces fit, choose between options, or avoid getting hurt?** If not, cut it. A true detail the
reader doesn't need is still noise. A good picture earns its place: servers as a small cloud of
your own, staged changes as a draft, a volume as a disk attached to one server.

## Who reads them

A developer with an app, coming from Railway, Heroku, Vercel or a VPS, who wants it online.
They know Git and environment variables. Many have never run a server, and they shouldn't
have to learn how Ployz schedules, routes, retries or caches.

## Voice

- Ployz is the confident subject; the reader is the one acting. "Ployz builds your app and
  gives it an https address." Present tense, active voice, "you".
- Warm and plain. Short paragraphs of one to three sentences.
- No hype and no cute lines: not "makes it easy", "simply", "seamless", "Ployz tells you where
  it hurts", "GitHub lets it down".

## Outcomes, not mechanism

Say what happens to the reader's app. Leave out how Ployz decides.

| Instead of | Write |
| --- | --- |
| "The newest admission replaces the pending attempt; the attempt holding the execution slot is never replaced" | "If you deploy again while a deployment is waiting, the newest one takes its place." |
| "The proxy picks a healthy replica at random, retries on another and skips it for 30 seconds" | "Each request goes to a healthy replica." |
| "Ployz rechecks the generated domain's records every hour" | "For up to an hour after a server goes down, some visitors may still be sent to it." |
| "Railpack 0.39.0 with BuildKit 0.26.2" | "Ployz builds your app with Railpack." |

Keep every outcome that bites (data loss, downtime, a deploy that fails, secrets leaving your
servers, surprise costs), phrased as what the reader sees, with what to do about it. Every
limit you mention comes with what to do instead.

## The reader's route

The docs are a route, not a shelf. A newcomer goes: **deploy your first app** (Quick start) →
**configure it** (variables, a database) → **test changes** (preview environments) → **get ready
for production**. Reference pages sit beside that route for anyone who jumps straight in.

Each general page has one job, and doesn't repeat the others:

| Page | Its job |
| --- | --- |
| Introduction | Help someone decide whether Ployz fits. |
| The basics | How your app is organized and how a change ships. |
| Quick start | Get one known app running, then point to one next step. |
| Feature pages | Do the tasks for one feature. |
| Service settings | One entry per field. The dashboard links here for more information. |
| Troubleshooting | Fix one symptom at a time. |

End a page with **one** recommended next step when there's an obvious one ("Next: add a
database"), not a list of four.

## Page shape

1. **Opening.** One or two sentences: what the feature does for you. When the idea is new,
   one more sentence that gives the reader a picture to hold, and at most one small Mermaid
   diagram, only where it replaces three or more sentences. No "Introduction" heading, no
   "What you get" list.
2. **Tasks.** H2s start with a verb ("Run more replicas", "Add a custom domain"), or name a
   situation the reader is in ("When a server goes down", "When a build fails"). Each task
   has at most four numbered steps with exact UI labels in bold, then one screenshot, then at
   most three sentences on what happens. Put any decision the reader must make (a port, an
   address, a volume type) **before** the step that commits it, usually **Deploy**. Optional
   tuning comes after.
3. **Good to know** (optional, last). At most five bullets, each with a bold lead-in, only for
   things that bite.

Most pages run 300–700 words. Troubleshooting pages can run longer because they are a
collection of short entries.

## Settings reference

[Service settings](services/settings.md) is the page the dashboard links to from its settings.
One H3 per field, and the heading is the field's **exact dashboard label**, so its link is
stable (`services/settings.md#watch-paths`). Under it, one to three sentences: what it does,
its default, when you'd change it, and a link to the feature page for more. Never rename one
of these headings without updating the dashboard's link.

## The CLI

Write for the dashboard. People use the dashboard; coding agents learn the CLI on their own
(`ployz setup agent`, `ployz --help`). So feature pages have no "From the CLI" blocks.

The one exception is something the dashboard can't do yet. Then show the command, say so
plainly, and keep it short:

```sh
# Server roles other than builds are CLI-only for now
ployz server set db-1 --accepts-ingress=false
```

Everything else about the CLI lives on the one [CLI](cli/overview.md) page.

## Troubleshooting

H2s are symptoms in the reader's words ("My app shows 502 Bad Gateway"). Put the exact
message on the first line under the heading, so search finds it. Then the fix, most common
cause first, in a few bullets or steps. No explanations of why Ployz works that way beyond
one sentence of "what it means".

## Words

Use the words the dashboard shows. Server (never machine, node, host or cluster),
organization, project, environment, service, replica, volume, deployment, variable, builder,
build order. Branch for an environment made from another, Git branch for Git's. Preview
environment (the dashboard says PR environments). Staged changes, Deploy, Publish.

Never write: Config Store, Engine, Namespace, Saved/Applied/Working State, ingress, Management
Client, Cloud Pairing, Founding Claim, iroh, NATS, Corrosion, BuildKit, OIDC. Exact quoted UI or
error text is the one exception; tell the product team when it uses these words.

## Elements

- **Screenshots:** PNGs in `images/`, dark theme, from the demo project. Use a full-window
  shot to orient the reader (where am I?) and a focused shot of the panel or dialog for a
  specific step. No large empty canvas, nothing blurry. Redact tokens, secrets, IPs and emails
  first. Where one belongs but doesn't exist yet, leave `<!-- screenshot: what it shows -->`.
- **Example names:** one app throughout. Project `my-app`, environments `production` and
  `staging`, services `web` and `postgres` (and `worker` when you need a third). The quick
  start uses the example repo `render-examples/express-hello-world`, so its service is
  `express-hello-world`.
- **Callouts:** `> [!NOTE]`, `> [!TIP]`, `> [!WARNING]`. At most one per page, for data loss,
  downtime or security.
- **Generated files** (like a workflow file Ployz writes for you) go in a `<details>` block.
- **Code:** `sh` blocks, no `$` prompts, the example names above and `203.0.113.10`.
- **Tables:** for choices and reference values, not prose.
- **Links:** relative, with `.md`. End with **Related** only when there's an obvious next page.

## Accuracy

Every label, default and command must be true: check the dashboard (`dashboard/src`) and the
CLI (`ployz <command> --help`). Accuracy is not completeness.
