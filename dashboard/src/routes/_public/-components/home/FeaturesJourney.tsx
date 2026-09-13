type Benefit = {
  title: string
  description: string
}

const benefits: ReadonlyArray<Benefit> = [
  {
    title: 'See the whole environment.',
    description:
      'Build and connect your stack on a visual canvas. Services, data, networking, and configuration stay visible at a glance.',
  },
  {
    title: 'Deploy without the platform work.',
    description:
      'Connect a repo, choose where it runs, and push. Ployz handles builds, configuration, health checks, previews, and rollout—without a new stack to learn.',
  },
  {
    title: 'Networking is ready when your app is.',
    description:
      'Private connections, public endpoints, TLS, and load balancing come together with the environment instead of becoming a separate project.',
  },
  {
    title: 'Grow without a platform rewrite.',
    description:
      'Start on a single Linux server. Add cloud regions, bare metal, or a home lab later without changing how your team ships.',
  },
  {
    title: 'See problems where you ship.',
    description:
      'Logs, metrics, health, alerts, and deployment history live together, so you can see what changed and what happened next.',
  },
  {
    title: 'Give every change room to breathe.',
    description:
      'Spin up an environment for every pull request, review it with isolated data, and roll back the app and its state together when you need to.',
  },
]

export function FeaturesJourney() {
  return (
    <section id="deploy" className="marketing-frame benefit-stack">
      <ul>
        {benefits.map((benefit, index) => (
          <li key={benefit.title} className="benefit-row">
            <div className="benefit-row__copy">
              <h3>{benefit.title}</h3>
              <p>{benefit.description}</p>
            </div>
            {index === 0 ? <EnvironmentCanvas /> : null}
          </li>
        ))}
      </ul>
    </section>
  )
}

function EnvironmentCanvas() {
  return (
    <figure className="benefit-canvas">
      <figcaption>One environment, visible on one canvas</figcaption>
      <svg
        viewBox="0 0 720 320"
        role="img"
        aria-label="Web and worker services connected to an API and database"
      >
        <path d="M145 160H300" />
        <path d="M420 115H575" />
        <path d="M420 205H575" />
        <path d="M360 145V175" />

        <g transform="translate(90 160)">
          <circle r="54" />
          <text>Web</text>
        </g>
        <g className="benefit-canvas__primary" transform="translate(360 100)">
          <circle r="60" />
          <text>API</text>
        </g>
        <g transform="translate(360 220)">
          <circle r="54" />
          <text>Worker</text>
        </g>
        <g transform="translate(630 160)">
          <circle r="58" />
          <text>Database</text>
        </g>
      </svg>
    </figure>
  )
}
