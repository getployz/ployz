use ployz_model::{
    ImageDistributePayload, ImageDistributeValidationPayload, ImageInspectPayload,
    ImagePushPayload, ImageReceiveSessionPayload, ImageReceivedImportPayload,
};

#[derive(Debug, Clone)]
pub enum ImageServicePayload {
    ImageInspect(ImageInspectPayload),
    ImagePush(ImagePushPayload),
    ImageDistribute(ImageDistributePayload),
    ImageDistributeValidation(ImageDistributeValidationPayload),
    ImageReceiveSession(ImageReceiveSessionPayload),
    ImageReceivedImport(ImageReceivedImportPayload),
}

#[derive(Debug, Clone)]
pub enum ImageServiceResponse {
    Success {
        code: String,
        message: String,
        payload: Option<ImageServicePayload>,
    },
    Error {
        code: String,
        message: String,
        payload: Option<ImageServicePayload>,
    },
}

impl ImageServiceResponse {
    #[must_use]
    pub fn success(message: impl Into<String>, payload: Option<ImageServicePayload>) -> Self {
        Self::Success {
            code: "OK".into(),
            message: message.into(),
            payload,
        }
    }

    #[must_use]
    pub fn error(
        code: impl Into<String>,
        message: impl Into<String>,
        payload: Option<ImageServicePayload>,
    ) -> Self {
        Self::Error {
            code: code.into(),
            message: message.into(),
            payload,
        }
    }

    #[must_use]
    pub fn is_ok(&self) -> bool {
        matches!(self, Self::Success { .. })
    }

    #[must_use]
    pub fn code(&self) -> &str {
        match self {
            Self::Success { code, .. } | Self::Error { code, .. } => code,
        }
    }

    #[must_use]
    pub fn message(&self) -> &str {
        match self {
            Self::Success { message, .. } | Self::Error { message, .. } => message,
        }
    }

    #[must_use]
    pub fn payload(&self) -> Option<ImageServicePayload> {
        match self {
            Self::Success { payload, .. } | Self::Error { payload, .. } => payload.clone(),
        }
    }

    #[must_use]
    pub fn into_payload(self) -> Option<ImageServicePayload> {
        match self {
            Self::Success { payload, .. } | Self::Error { payload, .. } => payload,
        }
    }
}
