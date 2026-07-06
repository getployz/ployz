# SQLite-Native Operation Store Spike

This is the shape if the operation store is designed around SQLite instead of
preserving the current JSONL + `working-records.json` implementation.

The lazy idea: one DB file owns operation events, current status, idempotency,
namespace fences, and short-lived machine-add escrow. Rust keeps domain
projection; SQL owns atomicity and uniqueness.

## Schema

```sql
pragma foreign_keys = on;
pragma journal_mode = wal;
pragma synchronous = normal;

create table operations (
  id text primary key,
  kind text not null,
  status_json text not null,
  last_sequence integer not null,
  terminal integer not null default 0,
  created_at_ms integer not null,
  updated_at_ms integer not null
);

create table operation_events (
  operation_id text not null references operations(id) on delete cascade,
  sequence integer not null,
  event_json text not null,
  recorded_at_ms integer not null,
  primary key (operation_id, sequence)
);

create table idempotency_keys (
  scope text not null,
  key text not null,
  operation_id text not null references operations(id) on delete cascade,
  primary key (scope, key),
  unique (scope, operation_id)
);

create table namespace_fences (
  namespace_id text primary key,
  operation_id text not null references operations(id) on delete cascade,
  acquired_at_ms integer not null
);

create table machine_add_claims (
  idempotency_key text primary key,
  operation_id text not null unique references operations(id) on delete cascade,
  machine_id text not null,
  claim_json text not null
);

create table machine_add_join_tokens (
  fingerprint text primary key,
  idempotency_key text not null references machine_add_claims(idempotency_key)
);

-- In-flight escrow, not operation evidence. Delete on terminal.
create table machine_add_escrow (
  idempotency_key text primary key references machine_add_claims(idempotency_key),
  raw_join_token text,
  secret_delivery_json text
);

create index operations_terminal_idx on operations(terminal);
create index operation_events_operation_idx on operation_events(operation_id, sequence);
```

## Repository Shape

```rust
pub struct OperationStore {
    conn: rusqlite::Connection,
    progress: async_nats::Client,
}

impl OperationStore {
    pub fn open(path: &Path, progress: async_nats::Client) -> Result<Self, StoreError>;

    pub async fn submit_deploy(
        &mut self,
        command: DeploySubmitCommand,
    ) -> Result<AcceptedDeploySubmission, SubmitError>;

    pub async fn append(
        &mut self,
        operation_id: &OperationId,
        event: OperationEvent,
    ) -> Result<AppendOutcome, AppendError>;

    pub fn status(&self, operation_id: &OperationId) -> Option<OperationStatus>;

    pub fn statuses(&self) -> Vec<OperationStatus>;

    pub fn replay(
        &self,
        operation_id: &OperationId,
        start: EventSequence,
        limit: EventReplayLimit,
    ) -> Result<OperationEventReplayPage, ReplayError>;
}
```

No in-memory `statuses: BTreeMap`. No startup replay. `operations.status_json`
is the current projection.

## Submit Deploy

```rust
fn submit_deploy(&mut self, cmd: DeploySubmitCommand) -> Result<AcceptedDeploySubmission, SubmitError> {
    let tx = self.conn.transaction()?;

    if let Some(existing) = find_idempotent(&tx, "deploy", &cmd.idempotency_key)? {
        return accepted_deploy_from_row(&tx, existing);
    }

    acquire_namespace_fence(&tx, &cmd.target.namespace_id, &cmd.operation_id)?;

    let event = OperationEvent::DeploySubmitted {
        operation_id: cmd.operation_id.clone(),
        target: cmd.target.clone(),
    };
    let status = OperationStatus::deploy_accepted(
        cmd.operation_id.clone(),
        cmd.target.status_service_id(),
        event_sequence(1),
    );

    insert_operation(&tx, &cmd.operation_id, OperationKind::Deploy, &status)?;
    insert_event(&tx, &cmd.operation_id, event_sequence(1), &event)?;
    insert_idempotency(&tx, "deploy", &cmd.idempotency_key, &cmd.operation_id)?;
    tx.commit()?;

    publish_progress(event);
    Ok(AcceptedDeploySubmission { should_start_execution: true, ... })
}
```

The namespace fence is durable because it is a row. Releasing it is a delete in
the same store:

```sql
delete from namespace_fences
where namespace_id = ?1 and operation_id = ?2;
```

## Append Event

```rust
fn append(&mut self, operation_id: &OperationId, event: OperationEvent) -> Result<AppendOutcome, AppendError> {
    let tx = self.conn.transaction()?;
    let current = load_status_for_update(&tx, operation_id)?;
    let next = current.next_event_sequence();

    let projection = project_operation_event(&current, event.clone(), next)?;
    let OperationProjection::StatusChanged { status } = projection else {
        tx.commit()?;
        return Ok(AppendOutcome::AlreadySatisfied {
            current_sequence: current.last_event_sequence(),
        });
    };

    insert_event(&tx, operation_id, next, &event)?;
    update_operation_status(&tx, operation_id, status.as_ref(), next)?;

    if status.is_terminal() {
        release_fences_for_operation(&tx, operation_id)?;
        delete_machine_add_escrow_for_operation(&tx, operation_id)?;
    }

    tx.commit()?;
    publish_progress(event);
    Ok(AppendOutcome::Stored { sequence: next })
}
```

This replaces:

- reread JSONL to find the next sequence
- append file fsync code
- update in-memory status map
- remember to delete escrow in each terminal caller
- remember to release namespace locks in the runtime wrapper

## Machine Add

Machine-add becomes explicit rows instead of a packed JSON index:

```rust
fn submit_machine_add(&mut self, cmd: MachineAddSubmitCommand) -> Result<AcceptedMachineAddSubmission, SubmitError> {
    let tx = self.conn.transaction()?;

    if let Some(existing) = find_idempotent(&tx, "machine_add", &cmd.idempotency_key)? {
        return accepted_machine_add_from_rows(&tx, existing);
    }

    insert_machine_add_claim(&tx, &cmd)?;
    insert_join_token(&tx, &cmd.join_token.fingerprint(), &cmd.idempotency_key)?;
    insert_machine_add_escrow(&tx, &cmd.idempotency_key, &cmd.raw_join_token, &cmd.join_secret_delivery)?;

    let event = machine_add_submitted_event(&cmd);
    let status = machine_add_pending_status(&cmd, event_sequence(1));
    insert_operation(&tx, &cmd.operation_id, OperationKind::MachineAdd, &status)?;
    insert_event(&tx, &cmd.operation_id, event_sequence(1), &event)?;
    insert_idempotency(&tx, "machine_add", &cmd.idempotency_key, &cmd.operation_id)?;

    tx.commit()?;
    publish_progress(event);
    accepted_machine_add_from_rows(&self.conn, cmd.operation_id)
}
```

Operation-id collision is SQL, not Rust scans:

```sql
operation_id text not null unique
```

Join token collision is SQL:

```sql
fingerprint text primary key
```

Terminal scrubbing is SQL:

```sql
delete from machine_add_escrow
where idempotency_key in (
  select idempotency_key from machine_add_claims where operation_id = ?1
);
```

## What Disappears

- `OperationIndex`
- `recover_machine_add_submissions`
- `load_statuses`
- per-startup JSONL projection replay
- `event_sequence_from_index`
- most `create_or_adopt`
- in-memory namespace lock map
- hand-coded operation-id collision scans
- terminal callers remembering escrow cleanup
- impossible read errors for `BTreeMap` reads

## What Stays

- domain event/status types
- `project_operation_event`
- API-specific accepted response builders
- NATS progress publish
- small error mapping layer

## Verdict

This is simpler if we replace the store boundary. It is not simpler if we keep
JSONL as the real store and add SQLite beside it.

