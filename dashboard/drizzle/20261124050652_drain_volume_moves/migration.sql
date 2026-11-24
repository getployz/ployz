ALTER TABLE "server_drain_attempt" DROP CONSTRAINT "server_drain_attempt_state_shape_check", ADD CONSTRAINT "server_drain_attempt_state_shape_check" CHECK ((
        ("state" = 'pending'
          and "started_at" is null and "ended_at" is null and "report" is null and "end_code" is null)
        or ("state" = 'running' and "inngest_run_id" is not null
          and "started_at" is not null and "ended_at" is null
          and ("report" is null or jsonb_typeof("report") = 'object') and "end_code" is null)
        or ("state" = 'finished' and "inngest_run_id" is not null
          and "started_at" is not null and "ended_at" is not null
          and jsonb_typeof("report") = 'object' and "end_code" is null)
        or ("state" in ('failed', 'cancelled')
          and "ended_at" is not null and "report" is null and "end_code" is not null)
        or ("state" = 'unknown' and "inngest_run_id" is not null
          and "started_at" is not null and "ended_at" is not null and "report" is null
          and "end_code" is not null)
      ));