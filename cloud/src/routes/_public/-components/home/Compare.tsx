const comparisons = [
  ['Git deployments', 'Repository deploys with automatic deploys', 'Repository deploys to your servers'],
  ['PR environments', 'Temporary environment per pull request', 'Temporary environment with isolated data'],
  ['Workload location', 'Railway-managed regions', 'Servers you connect'],
  ['Rollback scope', 'Previous image and custom variables', 'App release and database snapshot'],
  ['Operating model', 'Hosted platform', 'Managed dashboard or open-source core'],
] as const

export function Compare() {
  return (
    <section id="compare" className="marketing-comparison">
      <div className="marketing-frame marketing-section-head">
        <div>
          <h2>The familiar workflow, with a different boundary.</h2>
          <p>
            If Railway is the workflow you know, Ployz changes where the
            workloads run and how much of the stack you can own.
          </p>
        </div>
      </div>

      <div className="marketing-frame">
        <table className="marketing-comparison__table">
          <caption className="sr-only">
            Railway and Ployz deployment boundary comparison
          </caption>
          <thead>
            <tr className="marketing-comparison__header">
              <th scope="col">Boundary</th>
              <th scope="col">Railway</th>
              <th scope="col">Ployz</th>
            </tr>
          </thead>
          <tbody>
            {comparisons.map(([label, railway, ployz]) => (
              <tr key={label} className="marketing-comparison__row">
                <th scope="row">{label}</th>
                <td>
                  <small>Railway</small>
                  {railway}
                </td>
                <td>
                  <small>Ployz</small>
                  {ployz}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>

      <p className="marketing-frame marketing-comparison__sources">
        Railway capabilities checked against its official{' '}
        <a href="https://docs.railway.com/deployments/deployment-actions">
          deployment actions
        </a>{' '}
        and <a href="https://docs.railway.com/environments">environments</a>{' '}
        documentation.
      </p>
    </section>
  )
}
