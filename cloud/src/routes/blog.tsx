import { createFileRoute } from '@tanstack/react-router'

export const Route = createFileRoute('/blog')({
  component: BlogPage,
})

function BlogPage() {
  return (
    <main>
      <section className="pb-16 pt-20">
        <div className="page-wrap">
          <p className="section-label mb-3">Blog</p>
          <h1 className="mb-5 text-4xl font-extrabold tracking-tight sm:text-5xl">
            Updates & insights
          </h1>
          <p className="max-w-xl text-lg text-[var(--plz-text-muted)]">
            Engineering deep-dives, release notes, and the thinking behind
            Ployz.
          </p>

          <div className="mt-16 text-center">
            <div className="card mx-auto max-w-md p-10">
              <p className="text-4xl">🚧</p>
              <p className="mt-4 text-base font-semibold text-[var(--plz-text)]">
                Coming soon
              </p>
              <p className="mt-2 text-sm text-[var(--plz-text-muted)]">
                We're working on our first posts. Check back soon or follow us
                on GitHub for updates.
              </p>
            </div>
          </div>
        </div>
      </section>
    </main>
  )
}
