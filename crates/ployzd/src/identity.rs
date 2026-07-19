pub(crate) fn prefix_id(prefix: &str, id: &str) -> String {
    format!("{prefix}{id}")
}

#[cfg(test)]
mod tests {
    use super::prefix_id;

    #[test]
    fn generated_identity_prefixes_preserve_case_distinct_nuids() {
        for prefix in ["", "upload_", "ployz-image-", "ployz-image-upload-"] {
            let upper = prefix_id(prefix, "A1");
            let lower = prefix_id(prefix, "a1");

            assert_eq!(upper, format!("{prefix}A1"));
            assert_eq!(lower, format!("{prefix}a1"));
            assert_ne!(upper, lower);
        }
    }
}
