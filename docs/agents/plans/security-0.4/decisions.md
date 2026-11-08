# Security 0.4 decisions

Choices made while shipping the program's slices, with what each leaves open. Read the bets in
`core/DESIGN.md` first; this file records only what they didn't settle.

## B3: proposals

**A Machine verifies that its step is in the bundle, not the order across Machines.** A step's
envelope rides inside the one bundle the approver's browser signed, and the Machine checks the
step's digest sits at the index the envelope names, that its kind matches, and that the window,
Cluster and namespace are the bundle's. Each step is consumed once per Machine and Container, by
its index. So Cloud can't add a step, change one, or run one the person didn't approve.

Residual: Cloud could still run the approved steps out of order, or stop before the last one.
No Machine sees the other Machines' steps end, so none can refuse step 3 because step 2 never
ran. The card and `proposal_status` show where each step got, which is how a person notices.
Closing this would mean each Machine learning the earlier steps' ends through a signed chain,
which the program's remaining slices don't need.
