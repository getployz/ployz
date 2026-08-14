use bollard::{Docker, errors::Error, query_parameters::CreateImageOptionsBuilder};
use futures_util::TryStreamExt;
use ployz_core::PullPolicy;

pub(crate) async fn prepare_image(
    docker: &Docker,
    image: &str,
    policy: PullPolicy,
) -> Result<(), Error> {
    let pull = match policy {
        PullPolicy::Always => true,
        PullPolicy::Never => false,
        PullPolicy::Missing => match docker.inspect_image(image).await {
            Ok(_) => false,
            Err(error) if is_not_found(&error) => true,
            Err(error) => return Err(error),
        },
    };
    if pull {
        docker
            .create_image(
                Some(
                    CreateImageOptionsBuilder::default()
                        .from_image(image)
                        .build(),
                ),
                None,
                None,
            )
            .try_collect::<Vec<_>>()
            .await?;
    }
    Ok(())
}

pub(crate) fn is_not_found(error: &Error) -> bool {
    matches!(
        error,
        Error::DockerResponseServerError {
            status_code: 404,
            ..
        }
    )
}
