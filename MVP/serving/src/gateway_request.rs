use hyper::http::{HeaderMap, HeaderValue, header::HOST};

const ACME_HTTP01_PREFIX: &str = "/.well-known/acme-challenge/";

pub(crate) fn acme_http01_token(path: &str) -> Option<&str> {
    let token = path.strip_prefix(ACME_HTTP01_PREFIX)?;
    if token.is_empty() || token.contains('/') {
        return None;
    }
    Some(token)
}

pub(crate) fn request_host(headers: &HeaderMap<HeaderValue>) -> Option<String> {
    headers
        .get(HOST)
        .and_then(|value| value.to_str().ok())
        .map(strip_port)
        .filter(|value| !value.trim().is_empty())
}

fn strip_port(value: &str) -> String {
    let value = value.trim();
    if value.starts_with('[') {
        return value
            .split(']')
            .next()
            .map_or(value, |host| host.trim_start_matches('['))
            .to_string();
    }
    value.split(':').next().unwrap_or(value).to_string()
}

#[cfg(test)]
mod tests {
    use hyper::http::HeaderMap;

    use super::{acme_http01_token, request_host};

    #[test]
    fn acme_token_only_accepts_single_non_empty_path_segment() {
        assert_eq!(
            acme_http01_token("/.well-known/acme-challenge/token"),
            Some("token")
        );
        assert_eq!(acme_http01_token("/.well-known/acme-challenge/"), None);
        assert_eq!(
            acme_http01_token("/.well-known/acme-challenge/token/extra"),
            None
        );
    }

    #[test]
    fn request_host_normalizes_plain_and_ipv6_hosts() {
        let mut headers = HeaderMap::new();
        headers.insert("host", "Example.Test:443".parse().expect("host"));
        assert_eq!(request_host(&headers).as_deref(), Some("Example.Test"));

        headers.insert("host", "[fd00::1]:443".parse().expect("ipv6 host"));
        assert_eq!(request_host(&headers).as_deref(), Some("fd00::1"));
    }
}
