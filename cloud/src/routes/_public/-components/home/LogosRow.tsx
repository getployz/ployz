const infrastructure = ['Cloud VPS', 'Bare metal', 'Home lab'] as const

export function LogosRow() {
  return (
    <section
      className="benefit-infrastructure-strip"
      aria-label="Supported infrastructure"
    >
      <div className="marketing-frame benefit-infrastructure-strip__inner">
        <strong>One workflow. Your infrastructure.</strong>
        <ul>
          {infrastructure.map((item) => (
            <li key={item}>{item}</li>
          ))}
        </ul>
      </div>
    </section>
  )
}
