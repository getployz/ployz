//! The version a review or a refusal names, which `--expect-version` hands back.

use std::fmt;
use std::str::FromStr;

use crate::id::ProjectId;

/// A review's version is `REVISION:SAVED:TOKEN`, and a data-loss refusal's adds
/// `:DIGEST`. Cloud binds a review's version to the copy loss it refused with
/// `#loss-HASH`, which the server checks. A Project removal's is `PROJECT:DIGEST`.
/// It reads back as it was typed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReviewVersion(String);

/// Text that is none of the forms a review version takes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MalformedReviewVersion;

impl ReviewVersion {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ReviewVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for ReviewVersion {
    type Err = MalformedReviewVersion;

    fn from_str(version: &str) -> Result<Self, Self::Err> {
        let (reviewed, bound) = match version.split_once("#loss-") {
            Some((reviewed, hash)) => (reviewed, Some(hash)),
            None => (version, None),
        };
        let base36 = |hash: &str| {
            !hash.is_empty()
                && hash
                    .chars()
                    .all(|c| c.is_ascii_digit() || c.is_ascii_lowercase())
        };
        if bound.is_some_and(|hash| !base36(hash)) {
            return Err(MalformedReviewVersion);
        }
        let parts: Vec<&str> = reviewed.split(':').collect();
        match (parts.as_slice(), bound) {
            ([project, digest], None)
                if !digest.is_empty() && ProjectId::parse(*project).is_ok() =>
            {
                Ok(Self(version.to_owned()))
            }
            ([revision, saved, token, digests @ ..], _)
                if revision.parse::<u64>().is_ok()
                    && saved.parse::<u64>().is_ok()
                    && token.split_once('.').is_some_and(|(deployed, ended)| {
                        deployed.parse::<u64>().is_ok() && ended.parse::<u64>().is_ok()
                    })
                    && digests.iter().all(|digest| {
                        !digest.is_empty() && digest.chars().all(|c| c.is_ascii_hexdigit())
                    }) =>
            {
                Ok(Self(version.to_owned()))
            }
            _ => Err(MalformedReviewVersion),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ReviewVersion;

    #[test]
    fn it_reads_back_what_it_accepted_as_typed() {
        let project = "9a79b1a9-3c26-40dc-8cd0-3e5567c6daca:3764cac4355a8215";
        for good in [
            "3:1:0.1",
            "3:0:0.12:ab12cd",
            project,
            "5:1:0.1#loss-fzfjky",
            "3:0:0.12:ab12cd#loss-0",
            "+3:1:0.1",
            "003:01:0.01",
            "3:0:0.12:ab12cd:FF00",
            "3:1:0.1:ABCDEF#loss-0z",
            "18446744073709551615:0:0.0",
        ] {
            let version: ReviewVersion = good.parse().unwrap();
            assert_eq!(
                (version.as_str(), version.to_string()),
                (good, good.to_owned())
            );
        }
    }

    #[test]
    fn it_refuses_every_other_form() {
        for bad in [
            "garbage",
            "3:1",
            "3:1:",
            "x:1:0.1",
            "3:1:0.1:",
            "shop:ab12cd",
            "16:1:0.0XYZ",
            "16:1:0.0; rm -rf /",
            "16:1:0.0 trailing",
            "16:1:0.0/extra",
            "16:1:0",
            "3:0:0.12:ab12cd!",
            "5:1:0.1#loss-",
            "5:1:0.1#loss-FZ",
            "5:1:0.1#loss-fz; rm -rf /",
            "5:1:0.1#other-fzfjky",
            "5:1:0.1#loss-a#loss-b",
            "#loss-fzfjky",
            "9a79b1a9-3c26-40dc-8cd0-3e5567c6daca:3764cac4355a8215#loss-fzfjky",
            "18446744073709551616:0:0.0",
            "3:1:0.1:xyz",
            "3:1:+0.1.2",
            "3:1:0.1:ab:",
            "3:1:0.1 ",
            "3:1:0.1#loss-a b",
        ] {
            assert!(bad.parse::<ReviewVersion>().is_err(), "{bad}");
        }
    }
}
