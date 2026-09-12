ALTER TABLE "environment_deployment" DROP CONSTRAINT "environment_deployment_cancellation_shape_check", ADD CONSTRAINT "environment_deployment_cancellation_shape_check" CHECK ((
        ("status" = 'cancelled' and "cancellation_requested_at" is not null and "finished_at" is not null)
        or ("status" <> 'cancelled')
      ));