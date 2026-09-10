import { Link } from "@tanstack/react-router";
import { useState } from "react";
import {
  ArrowRight,
  Check,
  Compass,
  Flag,
  RotateCcw,
  Trophy,
} from "lucide-react";

const islands = [
  {
    name: "Home port",
    coordinate: "01 · YOUR FOUNDATION",
    title: "A ship of your own.",
    description:
      "Connect your Linux servers and give your containers somewhere to live. Ployz brings the deployment workflow; you keep the keys.",
    reward: "Ownership discovered",
    action: "Claim your ship",
    detail: "Your servers. Your applications. Your call.",
  },
  {
    name: "Fork island",
    coordinate: "02 · THE NEXT FRONTIER",
    title: "What if you could try anything?",
    description:
      "An environment for every idea. The direction we’re building toward: independent data clones, useful previews, and space for your AI crew to experiment.",
    reward: "Curiosity discovered",
    action: "Explore the possibilities",
    detail: "On the horizon: branching environments with cloned data.",
  },
  {
    name: "Open waters",
    coordinate: "03 · YOUR NEXT ADVENTURE",
    title: "Choose your own course.",
    description:
      "Start small. Add another machine when you need it. Bring your stack and your favourite tools. There’s a whole ocean beyond the default settings.",
    reward: "Freedom discovered",
    action: "Chart your course",
    detail: "Cloud VPS, bare metal, or your own rack.",
  },
] as const;

export function Voyage() {
  const [selected, setSelected] = useState(0);
  const [discovered, setDiscovered] = useState<number[]>([]);
  const island = islands[selected] ?? islands[0];
  const complete = discovered.length === islands.length;
  const claimed = discovered.includes(selected);

  function discover() {
    if (!claimed) setDiscovered([...discovered, selected]);
    const next = islands.findIndex(
      (_, index) => index !== selected && !discovered.includes(index),
    );
    if (next !== -1) setSelected(next);
  }

  return (
    <section
      className="pirate-world pirate-frame"
      id="world"
      aria-label="Interactive Ployz exploration demo"
    >
      <div className="pirate-world-bar">
        <span>
          <Compass aria-hidden="true" /> THE OPEN SEA{" "}
          <span className="pirate-demo-label">INTERACTIVE DEMO</span>
        </span>
        <span className="pirate-world-progress">
          <Flag aria-hidden="true" /> {discovered.length} / 3 DISCOVERED
        </span>
      </div>
      <div className="pirate-game">
        <div className="pirate-map" aria-label="Choose an island to explore">
          <img
            src="/assets/pirate-world.png"
            alt="Pixel-art pirate archipelago with palm trees, a harbour, a lighthouse, and a little sailing ship"
            width="1536"
            height="1024"
            fetchPriority="high"
          />
          <div className="pirate-map-coordinate">
            PLOYZ ARCHIPELAGO
            <br />
            23° N · 42° W
          </div>
          {islands.map((entry, index) => (
            <button
              key={entry.name}
              type="button"
              className="pirate-map-pin"
              data-island={index}
              aria-pressed={selected === index}
              aria-controls="island-details"
              onClick={() => setSelected(index)}
            >
              <span className="pirate-pin-number">
                {discovered.includes(index) ? (
                  <Check aria-hidden="true" />
                ) : (
                  `0${index + 1}`
                )}
              </span>
              <span>{entry.name}</span>
            </button>
          ))}
          <span className="pirate-map-hint">
            CLICK AN ISLAND. FIND YOUR FREEDOM.
          </span>
          <span className="pirate-compass" aria-hidden="true">
            N<br />✥
          </span>
        </div>
        <div className="pirate-quest" id="island-details">
          <div className="pirate-quest-top">
            <span className="pirate-caption">YOUR QUEST LOG</span>
            <span>✦ {discovered.length * 100} XP</span>
          </div>
          <progress
            value={discovered.length}
            max={3}
            aria-label="Islands discovered"
          />
          <div className="pirate-quest-copy" aria-live="polite">
            {complete ? (
              <>
                <Trophy className="pirate-quest-icon" aria-hidden="true" />
                <p className="pirate-caption">ACHIEVEMENT UNLOCKED</p>
                <h2>
                  Captain of <br />
                  your own ship.
                </h2>
                <p>
                  Three discoveries. One idea: building software should feel
                  like this. Now give your next project a place to set sail.
                </p>
                <p className="pirate-quest-detail">
                  Tour complete. Your real adventure starts here.
                </p>
              </>
            ) : (
              <>
                <Compass className="pirate-quest-icon" aria-hidden="true" />
                <p className="pirate-caption">{island.coordinate}</p>
                <h2>{island.title}</h2>
                <p>{island.description}</p>
                <p className="pirate-quest-detail">{island.detail}</p>
              </>
            )}
          </div>
          {complete ? (
            <Link className="pirate-button" to="/auth">
              Climb aboard <ArrowRight aria-hidden="true" />
            </Link>
          ) : (
            <button className="pirate-button" type="button" onClick={discover}>
              {claimed ? "Keep exploring" : island.action}
              <ArrowRight aria-hidden="true" />
            </button>
          )}
          <div className="pirate-quest-bottom">
            <span aria-live="polite">
              {islands.find((_, index) => index === discovered.at(-1))
                ?.reward ?? "+100 XP PER DISCOVERY"}
            </span>
            <button
              type="button"
              aria-label="Reset adventure"
              onClick={() => {
                setDiscovered([]);
                setSelected(0);
              }}
            >
              <RotateCcw aria-hidden="true" />
            </button>
          </div>
        </div>
      </div>
      <div className="pirate-world-caption">
        <span>A SMALL TASTE OF A BIGGER WORLD.</span>
        <span>Explore here. Build on your own hardware.</span>
      </div>
    </section>
  );
}
