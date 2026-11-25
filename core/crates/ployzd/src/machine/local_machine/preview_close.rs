//! Closing a preview on its home Server. Closing is written first, so from then on
//! nothing of the preview is created, started or arrives here. Then its Containers go,
//! then its Volumes, and only then is it marked Closed. A Close that stops halfway stays
//! Closing, and a daemon start or another Close runs the whole sweep again.

use ployz_core::{Namespace, PreviewClosed, PreviewNamespace, PreviewRefusal};

use super::{Checked, Error, LocalMachine};

impl LocalMachine {
    /// Close `namespace`: an exact preview Namespace this Server opened. Anything else is
    /// refused before anything changes. Holds the switch, then the exclusive turn, so no
    /// create, start or copy of the preview is halfway through when Closing is written.
    ///
    /// # Errors
    /// Returns [`Error::PreviewRefused`] for a Namespace this Server never opened as a
    /// preview, and the error of a step that failed, which leaves the preview Closing.
    pub(crate) async fn close_preview(&self, namespace: Namespace) -> Result<PreviewClosed, Error> {
        let never = || {
            Error::PreviewRefused(PreviewRefusal::NeverOpened {
                namespace: namespace.clone(),
            })
        };
        let preview = PreviewNamespace::of(&namespace).ok_or_else(never)?;
        preview.admit_on(&self.record().id()).map_err(|_| never())?;
        let switch = self.lock_switch().await?;
        self.finish_mutation(move |local| async move {
            let _switch = switch;
            local.sweep_preview(preview.namespace()).await
        })
        .await
    }

    /// Run again the Close of every preview whose sweep a stop interrupted.
    pub(crate) async fn resume_preview_closes(&self) {
        let closing = match self.approvals.closing_previews() {
            Ok(closing) => closing,
            Err(error) => {
                tracing::warn!(error = %ployz_core::error_chain::inline(&error), "couldn't read which previews are closing");
                return;
            }
        };
        for namespace in closing {
            match self.close_preview(namespace.clone()).await {
                Ok(_) => tracing::info!(%namespace, "finished closing preview"),
                Err(error) => {
                    tracing::warn!(%namespace, error = %ployz_core::error_chain::inline(&error), "preview stays closing");
                }
            }
        }
    }

    /// Refuse a start of, or data arriving into, a preview whose Close began.
    ///
    /// # Errors
    /// Returns [`Error::PreviewRefused`] once its Close began.
    pub(crate) fn admit_preview_runs(&self, namespace: &Namespace) -> Result<(), Error> {
        if PreviewNamespace::of(namespace).is_none() {
            return Ok(());
        }
        self.approvals
            .preview_runs(namespace)?
            .map_err(Error::PreviewRefused)
    }
}

impl Checked {
    async fn sweep_preview(&self, namespace: &Namespace) -> Result<PreviewClosed, Error> {
        self.approvals
            .begin_close(namespace)?
            .map_err(Error::PreviewRefused)?;
        let containers = self.docker()?;
        containers.lifecycle().remove_namespace(namespace).await?;
        let mut closed: PreviewClosed = self
            .plugin()
            .call(
                crate::volume_copy::PREVIEW_CLOSE_ROUTE,
                &ployz_core::ClosePreviewRequest {
                    namespace: namespace.clone(),
                },
            )
            .await
            .map_err(Error::StoragePreparation)?;
        for (name, ployz) in containers.volumes().in_namespace(namespace).await? {
            if ployz {
                containers.volumes().forget_volume(&name).await?;
            } else {
                closed.kept.push(name);
            }
        }
        self.approvals.finish_close(namespace)?;
        Ok(closed)
    }
}
