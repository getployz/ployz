//! The error page every Ployz site answers with, and the two failures Ployz raises itself.

use std::fmt::Write as _;

/// Browsers get this page; other clients get JSON or plain text with the same code.
const ERROR_PAGE: &str = include_str!("error_page.html");

/// A failure Ployz answers itself. Its code is the error message Caddy hands to the
/// `error_page` snippet, and the `data-error` and `data-for` selectors of `error_page.html`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ErrorPage {
    /// Nothing is deployed at the hostname.
    NotDeployed,
    /// The hostname is published but has no endpoint to proxy to.
    NotRunning,
}

impl ErrorPage {
    const ALL: [Self; 2] = [Self::NotRunning, Self::NotDeployed];

    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::NotDeployed => "NOT_DEPLOYED",
            Self::NotRunning => "NOT_RUNNING",
        }
    }

    pub(super) const fn status(self) -> u16 {
        match self {
            Self::NotDeployed => 404,
            Self::NotRunning => 503,
        }
    }

    /// The site directive that answers with this page.
    pub(super) fn directive(self) -> String {
        format!("\terror {} {}\n", self.as_str(), self.status())
    }

    fn message(self) -> &'static str {
        match self {
            Self::NotDeployed => "Nothing is deployed at this domain.",
            Self::NotRunning => "The app isn't running.",
        }
    }

    fn docs(self) -> &'static str {
        match self {
            Self::NotDeployed => "#nothing-is-deployed-here",
            Self::NotRunning => "#this-app-isnt-running",
        }
    }
}

/// The `error_page` snippet: one code per failure, negotiated as HTML, JSON or plain text.
///
/// Ployz's own errors carry their code as the message. Any proxy failure is `NO_RESPONSE`: a
/// refused or timed-out dial is 502, then passive health empties the pool and the rest are 503.
pub(super) fn write_error_page(output: &mut String) {
    output.push_str(
        r##"(error_page) {
	handle_errors {
		map "{err.status_code} {err.message}" {ployz_error} {ployz_message} {ployz_docs} {
"##,
    );
    for page in ErrorPage::ALL {
        let _ = writeln!(
            output,
            "\t\t\t\"{} {code}\" {code} \"{}\" \"{}\"",
            page.status(),
            page.message(),
            page.docs(),
            code = page.as_str(),
        );
    }
    output.push_str(
        r##"			~^50[234] NO_RESPONSE "The app didn't respond." "#this-app-didnt-respond"
			default INTERNAL_ERROR "Something went wrong." ""
		}
		map {http.request.host} {ployz_host} {
			~^([a-zA-Z0-9.:-]+)$ "${1}"
			default "this domain"
		}
		# Headers set on the failed route do not reach this response.
		import ployz_headers {args[0]}
		header {
			ployz-error {ployz_error}
			cache-control no-store
		}
		@not_running vars {ployz_error} "##,
    );
    output.push_str(ErrorPage::NotRunning.as_str());
    output.push_str(
        r##"
		header @not_running {
			retry-after 10
			refresh 10
		}
		@json header Accept *json*
		handle @json {
			header content-type application/json
			respond `{"error":{"status":{err.status_code},"code":"{ployz_error}","message":"{ployz_message}","request_id":"{http.request.uuid}","docs":"https://ployz.dev/docs/troubleshooting/app-not-reachable?utm_source=error-page&utm_medium=json&utm_campaign={ployz_error}&utm_content=docs{ployz_docs}"}}` {err.status_code}
		}
		@html header Accept *text/html*
		handle @html {
			header content-type "text/html; charset=utf-8"
			respond <<ERROR_PAGE
"##,
    );
    output.push_str(ERROR_PAGE);
    output.push_str(
        r##"ERROR_PAGE {err.status_code}
		}
		handle {
			header content-type "text/plain; charset=utf-8"
			respond <<TEXT
				{err.status_code} {ployz_error}: {ployz_message}
				Request: {http.request.uuid}
				https://ployz.dev/docs/troubleshooting/app-not-reachable?utm_source=error-page&utm_medium=text&utm_campaign={ployz_error}&utm_content=docs{ployz_docs}

				TEXT {err.status_code}
		}
	}
}

"##,
    );
}

#[cfg(test)]
mod tests {
    use super::{ERROR_PAGE, ErrorPage, write_error_page};

    #[test]
    fn a_page_is_its_code_status_and_directive() {
        assert_eq!(ErrorPage::NotDeployed.as_str(), "NOT_DEPLOYED");
        assert_eq!(ErrorPage::NotDeployed.status(), 404);
        assert_eq!(
            ErrorPage::NotDeployed.directive(),
            "\terror NOT_DEPLOYED 404\n"
        );
        assert_eq!(ErrorPage::NotRunning.as_str(), "NOT_RUNNING");
        assert_eq!(ErrorPage::NotRunning.status(), 503);
        assert_eq!(
            ErrorPage::NotRunning.directive(),
            "\terror NOT_RUNNING 503\n"
        );
    }

    #[test]
    fn the_snippet_maps_each_page_and_retries_only_a_stopped_app() {
        let mut snippet = String::new();
        write_error_page(&mut snippet);
        assert!(snippet.contains(concat!(
            "\t\tmap \"{err.status_code} {err.message}\" {ployz_error} {ployz_message} {ployz_docs} {\n",
            "\t\t\t\"503 NOT_RUNNING\" NOT_RUNNING \"The app isn't running.\" \"#this-app-isnt-running\"\n",
            "\t\t\t\"404 NOT_DEPLOYED\" NOT_DEPLOYED \"Nothing is deployed at this domain.\" \"#nothing-is-deployed-here\"\n",
            "\t\t\t~^50[234] NO_RESPONSE ",
        )));
        assert!(snippet.contains("\t\t@not_running vars {ployz_error} NOT_RUNNING\n"));
        assert!(snippet.contains(&format!("respond <<ERROR_PAGE\n{ERROR_PAGE}ERROR_PAGE ")));
    }

    #[test]
    fn the_html_shows_each_page() {
        for page in ErrorPage::ALL {
            let code = page.as_str();
            assert!(
                ERROR_PAGE.contains(&format!("[data-error=\"{code}\"]")),
                "{code}"
            );
            assert!(
                ERROR_PAGE.contains(&format!("data-for=\"{code}\"")),
                "{code}"
            );
        }
    }
}
