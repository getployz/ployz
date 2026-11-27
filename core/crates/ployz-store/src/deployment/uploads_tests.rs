use super::{ServiceUploads, UploadChoice};

#[test]
fn stored_service_uploads_read_back_and_write_the_same_bytes() {
    let digest = "d".repeat(64);
    let stored = [
        (
            format!(r#"{{"sent":true,"services":{{"web":"{digest}"}}}}"#),
            UploadChoice::Sent,
        ),
        (
            format!(r#"{{"sent":false,"provisional":true,"services":{{"web":"{digest}"}}}}"#),
            UploadChoice::AtStart,
        ),
        (
            format!(r#"{{"sent":false,"services":{{"api":"{digest}","web":"{digest}"}}}}"#),
            UploadChoice::Fixed,
        ),
    ];
    for (uploads, choice) in stored {
        let read: ServiceUploads = serde_json::from_str(&uploads).unwrap();
        assert_eq!(read.choice, choice);
        assert_eq!(serde_json::to_string(&read).unwrap(), uploads);
    }
    let both = format!(r#"{{"sent":true,"provisional":true,"services":{{"web":"{digest}"}}}}"#);
    assert!(serde_json::from_str::<ServiceUploads>(&both).is_err());
}
