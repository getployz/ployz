import { Link } from "@tanstack/react-router";
import {
  Anchor,
  ArrowDown,
  ArrowRight,
  Flag,
  Skull,
  Terminal,
} from "lucide-react";
import { GitHubMarkIcon } from "#/components/icons/github-mark";
import { Voyage } from "./Voyage";

const equipment = [
  {
    number: "01",
    title: "Bring your own ship.",
    copy: "A cloud VPS, bare metal, the machine in your cupboard. Run your containers on Linux servers you control.",
    item: "YOUR HARDWARE",
    icon: Anchor,
  },
  {
    number: "02",
    title: "Pack your whole stack.",
    copy: "Apps, workers, databases, persistent storage. Give your software a proper home, with room to try something new.",
    item: "YOUR STACK",
    icon: Terminal,
  },
  {
    number: "03",
    title: "Keep the captain’s chair.",
    copy: "An open-source engine under the hood. A clear view of what happened. Your infrastructure stays yours.",
    item: "YOUR RULES",
    icon: Flag,
  },
];

export function HomePage() {
  return (
    <div className="pirate-home">
      <a className="pirate-skip" href="#adventure">
        Skip to adventure
      </a>
      <header className="pirate-nav pirate-frame">
        <Link to="/home" className="pirate-wordmark" aria-label="Ployz home">
          <Skull aria-hidden="true" />
          ployz<span>™</span>
        </Link>
        <nav aria-label="Primary" className="pirate-nav-links">
          <a href="#world">The world</a>
          <a href="#equipment">Your loadout</a>
          <Link to="/docs">Field guide ↗</Link>
        </nav>
        <Link className="pirate-login" to="/auth">
          Climb aboard <ArrowRight aria-hidden="true" />
        </Link>
      </header>
      <main id="adventure">
        <section className="pirate-hero pirate-frame">
          <div className="pirate-eyebrow">
            <span aria-hidden="true">✳</span> OPEN-SOURCE INFRASTRUCTURE.
            OPEN-WORLD ENERGY.
          </div>
          <h1>
            Your ship.
            <br />
            Your rules.
            <span className="pirate-star" aria-hidden="true">
              ✳
            </span>
          </h1>
          <div className="pirate-hero-bottom">
            <p>
              Remember when computers were fun?
              <br />
              Deploy your stack. Explore a wild idea.
              <br />
              Make yourself at home on your own servers.
            </p>
            <div className="pirate-hero-actions">
              <a className="pirate-button" href="#world">
                Start your adventure <ArrowDown aria-hidden="true" />
              </a>
              <span className="pirate-caption">
                A LITTLE EXPLORATION. NO SIGN-UP REQUIRED.
              </span>
            </div>
          </div>
        </section>
        <Voyage />
        <div className="pirate-manifesto">
          <span>OWN YOUR SERVERS</span>
          <span aria-hidden="true">✳</span>
          <span>TRY WEIRD IDEAS</span>
          <span aria-hidden="true">✳</span>
          <span>ENJOY THE VOYAGE</span>
          <span aria-hidden="true">✳</span>
        </div>
        <section className="pirate-equipment pirate-frame" id="equipment">
          <div className="pirate-section-heading">
            <p className="pirate-eyebrow">THE CAPTAIN’S LOADOUT</p>
            <h2>
              Serious tools.
              <br />
              Room to play.
            </h2>
            <p>
              Good primitives make great adventures possible.
              <br />
              Ployz puts them within reach.
            </p>
          </div>
          <div className="pirate-equipment-list">
            {equipment.map(({ number, title, copy, item, icon: Icon }) => (
              <article className="pirate-equipment-item" key={number}>
                <div className="pirate-item-icon">
                  <Icon aria-hidden="true" />
                </div>
                <span className="pirate-caption">
                  {number} / {item}
                </span>
                <h3>{title}</h3>
                <p>{copy}</p>
              </article>
            ))}
          </div>
        </section>
        <section className="pirate-end">
          <div className="pirate-frame pirate-end-inner">
            <Skull aria-hidden="true" />
            <p className="pirate-eyebrow">
              THE INTERNET NEEDS MORE LITTLE ADVENTURES.
            </p>
            <h2>
              Go make
              <br />
              some waves.
            </h2>
            <Link className="pirate-button" to="/auth">
              Start with Ployz <ArrowRight aria-hidden="true" />
            </Link>
            <a
              className="pirate-source"
              href="https://github.com/getployz/ployz"
            >
              <GitHubMarkIcon aria-hidden="true" /> Or inspect the ship. It’s
              open source.
            </a>
          </div>
        </section>
      </main>
      <footer className="pirate-footer pirate-frame">
        <Link className="pirate-wordmark" to="/home">
          <Skull aria-hidden="true" />
          ployz
        </Link>
        <span>Built for the joy of building.</span>
        <nav aria-label="Footer">
          <Link to="/docs">Docs</Link>
          <Link to="/pricing">Pricing</Link>
          <a href="https://github.com/getployz/ployz">GitHub ↗</a>
        </nav>
      </footer>
    </div>
  );
}
