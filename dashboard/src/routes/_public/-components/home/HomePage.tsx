import { Link } from "@tanstack/react-router";
import {
  Anchor,
  ArrowRight,
  Bot,
  ChevronDown,
  Copy,
  Eye,
  HardDrive,
  Map,
  RotateCcw,
  Telescope,
  Ship,
} from "lucide-react";
import { GitHubMarkIcon } from "#/components/icons/github-mark";
import { PirateFlag } from "#/components/icons/pirate-flag";
import { runtimeRepoHref } from "#/components/marketing/links";

// ponytail: nav mega-menu is pure CSS (hover + focus-within). No JS state.
// Vocabulary is "environment", never "branch". Only shipped features are listed.
const featureMenu = [
  {
    group: "The shipyard",
    items: [
      ["Canvas", "Drop a box. Get a service.", "#ways"],
      ["Compose import", "Bring compose.yaml. Keep it.", "#ways"],
      ["CLI", "One binary, every action", "#ways"],
      ["Agents", "Real power, short leash", "#agents"],
    ],
  },
  {
    group: "The open sea",
    items: [
      ["Your own servers", "VPS, bare metal, the box under the desk", "#own"],
      ["Environment clones", "Data included. Seconds.", "#clone"],
      ["Snapshots & rollback", "Every deploy is undoable", "#clone"],
      ["Promote", "Review to production, one click", "#clone"],
    ],
  },
  {
    group: "The captain's quarters",
    items: [
      ["Preview then apply", "Every deploy shows its plan", "#ship"],
      ["Ingress & TLS", "Public routes, private mesh", "#ship"],
      ["Logs & metrics", "Next to the deploy that caused them", "#ship"],
      ["Open source", "Inspect the hull any time", runtimeRepoHref],
    ],
  },
] as const;

const stacks = [
  "Node", "Bun", "Python", "Go", "Rust", "PHP", "Ruby", ".NET",
  "Postgres", "MySQL", "Redis", "anything with a Dockerfile",
] as const;

// ponytail: Upsun's "The specs" table. Numbers say "seconds" until a real run is on file.
const specs = [
  ["Environment clone", "Seconds, data included", "vs 30s to 24h elsewhere"],
  ["Rollback", "Any snapshot, seconds", "every deploy is undoable"],
  ["Clone storage", "0 GB until you write", "copy-on-write, ZFS underneath"],
  ["New service on live data", "One rope on the canvas", "staging or production database"],
] as const;

// ponytail: snippets are deliberately tiny. They prove there are many ways in, not teach the API.
const ways = [
  ["Compose", "terminal", `ployz deploy -f compose.yaml`],
  ["CLI", "terminal", `ployz host bootstrap  # add a server`],
  ["Agent", "transcript", `> promote review-42 → production\n  awaiting your approval…`],
] as const;

// ponytail: Upsun's "built for how teams actually ship". One sentence per card, written as a scene.
const scenes = [
  [Copy, "A migration looks scary.", "Clone production, run it there, throw the clone away."],
  [RotateCcw, "The deploy went sideways at 2am.", "Roll back to the last snapshot. Go back to bed."],
  [Ship, "New checkout service.", "Drop it on the canvas. Rope it to the production database."],
  [Bot, "The agent finished.", "It previewed. You read the plan. You press promote."],
  [Eye, "Someone asks what changed.", "Logs, metrics and history sit next to the deploy."],
  [HardDrive, "The bill arrives.", "It's the server you already own. That's the bill."],
] as const;

const compare = [
  ["Runs on", "Their regions", "Your YAML", "Your servers"],
  ["Environments", "App only", "Build it yourself", "Cloned, data included"],
  ["Rollback", "Redeploy and hope", "helm rollback, maybe", "Any snapshot, seconds"],
  ["Deploys", "A spinner", "kubectl and prayer", "A plan you read first"],
  ["Exit", "Rewrite", "Rewrite", "It's Docker. Walk away."],
] as const;

const plans = [
  ["Self-host", "$0", "Runtime, CLI, mesh, ingress. Yours."],
  ["Free", "$0", "Dashboard, unlimited servers, 2 environments."],
  ["Hobby", "$9", "Logs, backups, scheduled jobs, more environments."],
  ["Pro", "$29", "Unlimited everything, priority support."],
] as const;

const faq = [
  ["Is this Kubernetes?", "No. It's Docker machines, a deploy planner, a private mesh and a dashboard. If it runs a container, it runs Ployz."],
  ["What servers can I use?", "Any Linux box you can SSH into. Start on one, enrol more with one command."],
  ["How far along is it?", "Open beta. Deploy, preview and apply, clones, snapshots and rollback ship today. Review environments on every pull request are next."],
] as const;

const footerLinks: ReadonlyArray<[string, ReadonlyArray<[string, string]>]> = [
  ["Product", [["Environments", "#clone"], ["Agents", "#agents"], ["Features", "#ship"], ["Pricing", "/pricing"]]],
  ["Learn", [["Docs", "/docs"], ["Why own", "#own"], ["Questions", "#faq"]]],
  ["Source", [["Runtime", runtimeRepoHref], ["Install script", "https://ployz.sh"]]],
];

function SectionHead({ eyebrow, title, line }: { eyebrow: string; title: string; line: string }) {
  return (
    <div className="pirate-head">
      <p className="pirate-eyebrow">{eyebrow}</p>
      <h2>{title}</h2>
      <p>{line}</p>
    </div>
  );
}

// ponytail: static SVG diagram. No layout lib; three nodes and two edges is all it needs.
function CloneDiagram() {
  return (
    <svg className="pirate-diagram" viewBox="0 0 720 300" role="img" aria-label="Production clones into a review environment with its own copy of the data, and the new web service promotes back into production">
      <defs>
        <marker id="pirate-arrow" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="8" markerHeight="8" orient="auto-start-reverse">
          <path d="M0 0L10 5 0 10z" fill="var(--pirate-coral)" />
        </marker>
      </defs>
      <path d="M190 150 C 270 150, 270 230, 350 230" fill="none" stroke="var(--pirate-ink)" strokeWidth="2" strokeDasharray="6 6" />
      <path d="M480 190 C 520 130, 520 70, 555 70" fill="none" stroke="var(--pirate-coral)" strokeWidth="2.5" markerEnd="url(#pirate-arrow)" />
      <g className="pirate-node pirate-node--prod">
        <rect x="30" y="110" width="160" height="80" rx="4" />
        <text x="46" y="140" className="pirate-node-title">production</text>
        <text x="46" y="168" className="pirate-node-meta">web · api · db</text>
      </g>
      <g className="pirate-node pirate-node--preview">
        <rect x="350" y="190" width="190" height="80" rx="4" />
        <text x="366" y="220" className="pirate-node-title">review-42</text>
        <text x="366" y="248" className="pirate-node-meta">same data · new web</text>
      </g>
      <g className="pirate-node pirate-node--prod">
        <rect x="555" y="30" width="160" height="80" rx="4" />
        <text x="571" y="60" className="pirate-node-title">production</text>
        <text x="571" y="88" className="pirate-node-meta">after · secrets kept</text>
      </g>
      <text x="60" y="230" className="pirate-edge-label">clone · seconds</text>
      <text x="548" y="150" className="pirate-edge-label pirate-edge-label--coral">promote web</text>
    </svg>
  );
}

export function HomePage() {
  return (
    <div className="pirate-home">
      <a className="pirate-skip" href="#main">Skip to content</a>
      <header className="pirate-nav pirate-frame">
        <Link to="/home" className="pirate-wordmark" aria-label="Ployz home">
          <PirateFlag />
          ployz<span>™</span>
        </Link>
        <nav aria-label="Primary" className="pirate-nav-links">
          <div className="pirate-menu">
            <button type="button" aria-haspopup="true">
              Features <ChevronDown aria-hidden="true" />
            </button>
            <div className="pirate-menu-panel">
              {featureMenu.map(({ group, items }) => (
                <div key={group}>
                  <p className="pirate-caption">{group.toUpperCase()}</p>
                  {items.map(([title, blurb, href]) => (
                    <a key={title} href={href}>
                      <strong>{title}</strong>
                      <span>{blurb}</span>
                    </a>
                  ))}
                </div>
              ))}
            </div>
          </div>
          <a href="#agents">Agents</a>
          <a href="#own">Why own</a>
          <Link to="/pricing">Pricing</Link>
          <Link to="/docs">Docs</Link>
          <a href={runtimeRepoHref}>GitHub ↗</a>
        </nav>
        <Link className="pirate-login" to="/auth">
          Climb aboard <ArrowRight aria-hidden="true" />
        </Link>
      </header>

      <main id="main">
        <section className="pirate-hero pirate-frame">
          <p className="pirate-eyebrow">OPEN-SOURCE DEPLOYMENT. YOUR SERVERS. OPEN BETA.</p>
          <h1>
            Production-perfect
            <br />
            environments.
            <br />
            On your servers.
            <br />
            In seconds.
            <span className="pirate-star" aria-hidden="true">✳</span>
          </h1>
          <div className="pirate-hero-bottom">
            <p>Clone any environment, data included. Deploy it to hardware you own. Roll back when it goes sideways.</p>
            <div className="pirate-hero-actions">
              <Link className="pirate-button" to="/auth">
                Start deploying <ArrowRight aria-hidden="true" />
              </Link>
              <pre className="pirate-install"><code>curl -fsSL https://ployz.sh | sh</code></pre>
            </div>
          </div>
          <ul className="pirate-hero-stats" aria-label="At a glance">
            <li><strong>seconds</strong><span>to clone an environment, data included</span></li>
            <li><strong>0</strong><span>Kubernetes</span></li>
            <li><strong>1</strong><span>bill, and it's your server's</span></li>
          </ul>
        </section>

        <section className="pirate-frame pirate-map-section">
          <figure className="pirate-map">
            <img
              src="/assets/pirate-world.png"
              alt="A pixel-art archipelago: a fortified island for production, a harbour for staging, small islands for review environments, and a lighthouse watching all of it."
              width={1536}
              height={1024}
            />
            <span className="pirate-map-label" style={{ top: "14%", left: "44%" }}><Map aria-hidden="true" /> production</span>
            <span className="pirate-map-label" style={{ top: "52%", left: "64%" }}><Anchor aria-hidden="true" /> staging</span>
            <span className="pirate-map-label" style={{ top: "11%", left: "78%" }}><Copy aria-hidden="true" /> review-42 · cloned from prod</span>
            <span className="pirate-map-label" style={{ top: "60%", left: "73%" }}><Copy aria-hidden="true" /> review-38 · shares staging db</span>
            <span className="pirate-map-label" style={{ top: "18%", left: "8%" }}><Telescope aria-hidden="true" /> logs · metrics</span>
            <span className="pirate-map-label" style={{ top: "70%", left: "10%" }}><Bot aria-hidden="true" /> agent: promoting web…</span>
            <figcaption className="pirate-world-caption">
              <span>ONE CLUSTER. MANY ENVIRONMENTS.</span>
              <span>The islands are environments. The ship is an agent.</span>
            </figcaption>
          </figure>
          <ul className="pirate-stacks" aria-label="Supported stacks">
            {stacks.map((s) => <li key={s}>{s}</li>)}
          </ul>
        </section>

        <div className="pirate-manifesto">
          <span>OWN YOUR SERVERS</span><span aria-hidden="true">✳</span>
          <span>CLONE EVERYTHING</span><span aria-hidden="true">✳</span>
          <span>READ THE PLAN</span><span aria-hidden="true">✳</span>
          <span>LET THE CREW SAIL</span>
        </div>

        <section className="pirate-section pirate-frame" id="clone">
          <SectionHead eyebrow="CLONE" title="Clone full production. In seconds." line="Everything production has. Nothing it doesn't. ZFS underneath, so a terabyte clones as fast as a megabyte." />
          <div className="pirate-panel">
            <CloneDiagram />
          </div>
          <table className="pirate-compare pirate-specs">
            <caption className="sr-only">The specs</caption>
            <thead>
              <tr><th scope="col">Feature</th><th scope="col">Capability</th><th scope="col">Note</th></tr>
            </thead>
            <tbody>
              {specs.map(([feature, capability, note]) => (
                <tr key={feature}><th scope="row">{feature}</th><td>{capability}</td><td>{note}</td></tr>
              ))}
            </tbody>
          </table>
        </section>

        <section className="pirate-section pirate-frame" id="ways">
          <SectionHead eyebrow="BUILD" title="Drop a box. Get a service." line="Canvas, Compose, CLI or an agent. One model underneath, your call on the way in." />
          <div className="pirate-ways">
            <figure className="pirate-way pirate-way--canvas">
              <figcaption><strong>Canvas</strong><span className="pirate-caption">dashboard</span></figcaption>
              <svg viewBox="0 0 760 110" aria-hidden="true">
                <path d="M120 55H200M280 55H360M440 55H480M480 55V25H520M480 55V85H520M600 85H620V55H640" stroke="var(--pirate-ink)" strokeWidth="1.5" fill="none" />
                <rect x="40" y="35" width="80" height="40" rx="3" />
                <rect x="200" y="35" width="80" height="40" rx="3" className="pirate-canvas-primary" />
                <rect x="360" y="35" width="80" height="40" rx="3" />
                <rect x="520" y="5" width="80" height="40" rx="3" />
                <rect x="520" y="65" width="80" height="40" rx="3" />
                <rect x="640" y="35" width="80" height="40" rx="3" />
                <g className="pirate-canvas-text">
                  <text x="80" y="59">web</text><text x="240" y="59">api</text><text x="400" y="59">worker</text>
                  <text x="560" y="29">redis</text><text x="560" y="89">db</text><text x="680" y="59">pg-data</text>
                </g>
              </svg>
            </figure>
            {ways.map(([title, file, snippet]) => (
              <figure className="pirate-way" key={title}>
                <figcaption><strong>{title}</strong><span className="pirate-caption">{file}</span></figcaption>
                <pre><code>{snippet}</code></pre>
              </figure>
            ))}
          </div>
        </section>

        <section className="pirate-agents" id="agents">
          <div className="pirate-frame">
            <SectionHead eyebrow="THE CREW" title="Give agents real power. Then a short leash." line="Every action previews first, names any data loss, and stays inside its environment." />
            <div className="pirate-terminal" aria-label="An agent operating a Ployz environment">
              <div className="pirate-terminal-bar">
                <span className="pirate-caption">AGENT · review-42</span>
                <span className="pirate-caption">LIVE</span>
              </div>
              <pre><code>{`> preview      plan: update web, run migrate · data loss: none
> deploy       ✓ web.review-42.harbor.sh · 14s
> promote      → production · keeps 12 secrets, 2 volumes
               awaiting captain approval…`}</code></pre>
            </div>
            <ul className="pirate-pills pirate-pills--dark">
              <li>Preview before apply</li>
              <li>Data loss is named</li>
              <li>Scope is one environment</li>
              <li>You hold the promote button</li>
            </ul>
          </div>
        </section>

        <section className="pirate-section pirate-frame" id="ship">
          <SectionHead eyebrow="BUILT FOR HOW TEAMS ACTUALLY SHIP" title="Six Tuesdays. Zero drama." line="" />
          <div className="pirate-feature-grid">
            {scenes.map(([Icon, title, line]) => (
              <article className="pirate-feature" key={title}>
                <Icon aria-hidden="true" />
                <h3>{title}</h3>
                <p>{line}</p>
              </article>
            ))}
          </div>
        </section>

        <section className="pirate-own" id="own">
          <div className="pirate-frame">
            <p className="pirate-eyebrow">WHY OWN THE SHIP</p>
            <h2>The cloud was a rental.<br />Rent is due.</h2>
            <p className="pirate-own-line">A modern server is absurdly fast and absurdly cheap. Bring the €40 box, the rack, or the Ryzen you built for "machine learning".</p>
            <ul className="pirate-own-stats">
              <li><strong>1</strong><span>machine to start</span></li>
              <li><strong>0</strong><span>registries or YAML required</span></li>
              <li><strong>$0</strong><span>for the runtime, forever</span></li>
              <li><strong>100%</strong><span>of your data on your disks</span></li>
            </ul>
          </div>
        </section>

        <section className="pirate-section pirate-frame">
          <SectionHead eyebrow="CHOOSE YOUR VESSEL" title="Rented yacht. Aircraft carrier. Or a ship." line="Ployz is the boat in the middle: fast to sail, yours to keep." />
          <table className="pirate-compare">
            <caption className="sr-only">Ployz compared with hosted PaaS and Kubernetes</caption>
            <thead>
              <tr><th scope="col" /><th scope="col">Hosted PaaS</th><th scope="col">Kubernetes</th><th scope="col">Ployz</th></tr>
            </thead>
            <tbody>
              {compare.map(([label, paas, k8s, ployz]) => (
                <tr key={label}><th scope="row">{label}</th><td>{paas}</td><td>{k8s}</td><td>{ployz}</td></tr>
              ))}
            </tbody>
          </table>
        </section>

        <section className="pirate-section pirate-frame" id="pricing">
          <SectionHead eyebrow="THE TREASURY" title="Free to sail. Cheap to crew." line="The runtime is free forever. Cloud costs less than a round of drinks." />
          <div className="pirate-plans">
            {plans.map(([name, price, blurb]) => (
              <article className="pirate-plan" key={name}>
                <span className="pirate-caption">{name.toUpperCase()}</span>
                <strong>{price}<small>/mo</small></strong>
                <p>{blurb}</p>
              </article>
            ))}
          </div>
          <Link className="pirate-textlink" to="/pricing">Full pricing <ArrowRight aria-hidden="true" /></Link>
        </section>

        <section className="pirate-section pirate-frame pirate-faq" id="faq">
          <SectionHead eyebrow="PARLEY" title="Fair questions." line="" />
          <div className="pirate-faq-list">
            {faq.map(([q, a]) => (
              <details key={q}>
                <summary>{q} <ChevronDown aria-hidden="true" /></summary>
                <p>{a}</p>
              </details>
            ))}
          </div>
        </section>

        <section className="pirate-end">
          <div className="pirate-frame pirate-end-inner">
            <PirateFlag />
            <p className="pirate-eyebrow">THE INTERNET NEEDS MORE LITTLE ADVENTURES.</p>
            <h2>Your next deploy<br />deserves an undo.</h2>
            <Link className="pirate-button" to="/auth">Climb aboard <ArrowRight aria-hidden="true" /></Link>
            <a className="pirate-source" href={runtimeRepoHref}>
              <GitHubMarkIcon aria-hidden="true" /> Or inspect the ship. It's open source.
            </a>
          </div>
        </section>
      </main>

      <footer className="pirate-footer pirate-frame">
        <div className="pirate-footer-top">
          <div>
            <Link className="pirate-wordmark" to="/home"><PirateFlag />ployz</Link>
            <p>Built for the joy of building.</p>
          </div>
          {footerLinks.map(([heading, links]) => (
            <nav key={heading} aria-label={heading}>
              <p className="pirate-caption">{heading.toUpperCase()}</p>
              {links.map(([label, href]) =>
                href.startsWith("/") ? <Link key={label} to={href}>{label}</Link> : <a key={label} href={href}>{label}</a>,
              )}
            </nav>
          ))}
        </div>
        <div className="pirate-footer-bottom">
          <span>© {new Date().getFullYear()} Ployz. Your ship. Your rules.</span>
          <span>No Kubernetes was harmed. It was never here.</span>
        </div>
      </footer>
    </div>
  );
}
