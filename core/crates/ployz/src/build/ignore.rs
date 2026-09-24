//! Docker's build-context ignore rules, applied before any source leaves this host.
//!
//! Matching follows `moby/patternmatcher`: gitignore semantics differ, so no
//! gitignore library is used.

use std::{
    collections::BTreeSet,
    fs, io,
    os::unix::ffi::OsStrExt as _,
    path::{Path, PathBuf},
};

use regex::Regex;

use super::Error;

/// Context entries the build may read, plus the ignore file that selected them.
pub(super) struct Selection {
    pub paths: BTreeSet<PathBuf>,
    pub ignore: Vec<u8>,
}

/// The recipe whose ignore rules select a build context.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) enum Rules {
    /// A Dockerfile's own ignore file replaces the context's `.dockerignore`.
    Dockerfile(PathBuf),
    /// The config's `exclude` list follows the Docker patterns, and the config
    /// stays readable. `None` names the default `railpack.json`, which is
    /// optional however it is named.
    Railpack(Option<PathBuf>),
}

/// `<Dockerfile>.dockerignore`, which Docker prefers over the context's ignore file.
pub(super) fn own_ignore(dockerfile: &Path) -> PathBuf {
    let mut own = dockerfile.as_os_str().to_owned();
    own.push(".dockerignore");
    own.into()
}

/// Select the `context` entries a Build may read.
pub(super) fn select(context: &Path, rules: &Rules) -> Result<Selection, Error> {
    let (own, railpack) = match rules {
        Rules::Dockerfile(dockerfile) => (read_optional(&own_ignore(dockerfile))?, None),
        Rules::Railpack(config) => (None, Some(config.as_deref())),
    };
    // Paths kept even when a pattern excludes them.
    let mut controls = Vec::new();
    let ignore = match own {
        Some(own) => own,
        None => {
            // Docker keeps the active root ignore file, even if it excludes itself.
            controls.push(PathBuf::from(".dockerignore"));
            read_optional(&context.join(".dockerignore"))?.unwrap_or_default()
        }
    };
    if reserved(&ignore) {
        return Err(Error::Invalid(RESERVED.into()));
    }
    let mut patterns = ignore_file_patterns(&ignore);
    if let Some(config) = railpack {
        let (excludes, kept) = railpack_rules(context, config)?;
        patterns.extend(excludes);
        controls.extend(kept);
    }
    let matcher = Matcher::new(&patterns)?;
    let mut paths = BTreeSet::new();
    walk(context, Path::new(""), &matcher, &controls, &mut paths)
        .map_err(|error| Error::Io(format!("select build context: {error}")))?;
    Ok(Selection { paths, ignore })
}

fn read_optional(path: &Path) -> Result<Option<Vec<u8>>, Error> {
    match fs::read(path) {
        Ok(content) => Ok(Some(content)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(Error::Io(format!("read {}: {error}", path.display()))),
    }
}

/// The config's exclusions and the paths preparation must still read.
/// Railpack 0.39.0 appends configured exclusions after Docker patterns, so
/// config negations can re-include Docker-ignored source.
fn railpack_rules(
    context: &Path,
    config: Option<&Path>,
) -> Result<(Vec<String>, Vec<PathBuf>), Error> {
    let invalid = |message: &str| Error::Invalid(message.into());
    let default = Path::new("railpack.json");
    let config = config.unwrap_or(default);
    // Go's `filepath.IsLocal`: relative, and still inside once cleaned.
    // The config names a UTF-8 Service variable, so this conversion is exact.
    let cleaned = clean(&config.to_string_lossy());
    if config.as_os_str().is_empty()
        || cleaned.starts_with('/')
        || cleaned == ".."
        || cleaned.starts_with("../")
    {
        return Err(invalid(
            "Railpack configuration must stay inside build.context",
        ));
    }
    // Go's `filepath.Join` cleans lexically, as Railpack does.
    let path = context.join(&cleaned);
    let mut kept = vec![PathBuf::from(cleaned)];
    match path.canonicalize() {
        Ok(resolved) => kept.push(
            resolved
                .strip_prefix(context)
                .map_err(|_| invalid("Railpack configuration escapes build.context"))?
                .to_owned(),
        ),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(_) => return Err(invalid("cannot read Railpack configuration")),
    }
    let content = match fs::read_to_string(&path) {
        Ok(content) => content,
        // Railpack's default config is optional, even when named explicitly.
        Err(error) if error.kind() == io::ErrorKind::NotFound && config == default => {
            return Ok((Vec::new(), kept));
        }
        Err(_) => return Err(invalid("cannot read Railpack configuration")),
    };
    #[derive(serde::Deserialize)]
    struct Configured {
        #[serde(default)]
        exclude: Vec<String>,
    }
    // Railpack accepts JSON with comments and trailing commas.
    let configured: Configured = standard_json(&content)
        .and_then(|json| serde_json::from_str(&json).ok())
        .ok_or_else(|| invalid("invalid Railpack configuration"))?;
    if configured
        .exclude
        .iter()
        .any(|pattern| reserved(pattern.as_bytes()))
    {
        return Err(invalid(RESERVED));
    }
    Ok((configured.exclude, kept))
}

fn walk(
    root: &Path,
    relative: &Path,
    matcher: &Matcher,
    controls: &[PathBuf],
    included: &mut BTreeSet<PathBuf>,
) -> io::Result<()> {
    let entries = match fs::read_dir(root.join(relative)) {
        Ok(entries) => entries,
        // BuildKit ignores permission errors in excluded directories, including
        // those visited to look for negated patterns.
        Err(error)
            if error.kind() == io::ErrorKind::PermissionDenied
                && !relative.as_os_str().is_empty()
                && matcher.matches_or_parent_matches(relative) =>
        {
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    let mut names = entries
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<io::Result<Vec<_>>>()?;
    names.sort();
    for name in names {
        let path = relative.join(&name);
        if reserved(name.as_bytes()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{}: {RESERVED}", path.display()),
            ));
        }
        // A kept path keeps its parents traversable; the frontend applies the same patterns.
        let ignored = matcher.matches_or_parent_matches(&path)
            && !controls.iter().any(|control| control.starts_with(&path));
        let directory = fs::symlink_metadata(root.join(&path))?.is_dir();
        if ignored {
            // Only descend when an exception can select a descendant (BuildKit semantics).
            if directory && matcher.exception_can_match_descendant(&path) {
                walk(root, &path, matcher, controls, included)?;
            }
            continue;
        }
        for parent in path
            .ancestors()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            included.insert(parent.to_owned());
        }
        if directory {
            walk(root, &path, matcher, controls, included)?;
        }
    }
    Ok(())
}

/// Docker's `.dockerignore` reader: comments before trimming, a leading BOM,
/// cleaned paths, and anchoring at the context root.
fn ignore_file_patterns(content: &[u8]) -> Vec<String> {
    let content = decode(content);
    let content = content.strip_prefix('\u{feff}').unwrap_or(&content);
    content
        .lines()
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| {
            let line = line.trim();
            let (invert, pattern) = match line.strip_prefix('!') {
                Some(pattern) => (true, pattern.trim()),
                None => (false, line),
            };
            if line.is_empty() {
                return None;
            }
            let mut pattern = if pattern.is_empty() {
                String::new()
            } else {
                clean(pattern)
            };
            if pattern.len() > 1 && pattern.starts_with('/') {
                pattern.remove(0);
            }
            Some(if invert {
                format!("!{pattern}")
            } else {
                pattern
            })
        })
        .collect()
}

struct Matcher {
    patterns: Vec<Pattern>,
}

struct Pattern {
    source: String,
    exclusion: bool,
    kind: Kind,
}

/// How `patternmatcher` compares one pattern: wildcard-free forms compare as
/// strings, so only wildcard patterns reach a regex.
enum Kind {
    Exact,
    /// A trailing `**`: the literal before it is a prefix.
    Prefix(String),
    /// A leading `**`: the literal after it is a suffix.
    Suffix(String),
    Regex(Regex),
}

impl Pattern {
    fn matches(&self, path: &str) -> bool {
        match &self.kind {
            Kind::Exact => path == self.source,
            Kind::Prefix(prefix) => path.starts_with(prefix.as_str()),
            // `**/foo` also matches `foo`.
            Kind::Suffix(suffix) => {
                path.ends_with(suffix.as_str()) || suffix.strip_prefix('/') == Some(path)
            }
            Kind::Regex(regex) => regex.is_match(path),
        }
    }
}

impl Matcher {
    fn new(patterns: &[String]) -> Result<Self, Error> {
        let patterns = patterns
            .iter()
            .map(|pattern| pattern.trim())
            .filter(|pattern| !pattern.is_empty())
            .map(|pattern| {
                let pattern = clean(pattern);
                let (exclusion, source) = match pattern.strip_prefix('!') {
                    Some("") => {
                        return Err(Error::Invalid("illegal exclusion pattern: \"!\"".into()));
                    }
                    Some(source) => (true, source.to_owned()),
                    None => (false, pattern),
                };
                let kind = compile(&source).ok_or_else(|| {
                    Error::Invalid(format!("invalid build context ignore pattern: {source}"))
                })?;
                Ok(Pattern {
                    source,
                    exclusion,
                    kind,
                })
            })
            .collect::<Result<_, _>>()?;
        Ok(Self { patterns })
    }

    /// The last matching pattern decides, checked against the path and each parent.
    fn matches_or_parent_matches(&self, path: &Path) -> bool {
        let path = decode_path(path);
        let parents: Vec<&str> = path.split('/').collect();
        let mut matched = false;
        for pattern in &self.patterns {
            if pattern.exclusion != matched {
                continue;
            }
            let found = pattern.matches(&path)
                || (1..parents.len())
                    .any(|end| pattern.matches(&parents.get(..end).unwrap_or_default().join("/")));
            if found {
                matched = !pattern.exclusion;
            }
        }
        matched
    }

    /// A wildcard cannot match outside the literal prefix preceding it. Remaining
    /// cases stay conservative so `**` and escaped patterns keep descendants.
    fn exception_can_match_descendant(&self, directory: &Path) -> bool {
        let directory = format!("{}/", decode_path(directory));
        self.patterns
            .iter()
            .filter(|pattern| pattern.exclusion)
            .any(|pattern| {
                let prefix = pattern
                    .source
                    .find(['*', '[', ']', '?', '^', '\\'])
                    .map_or(pattern.source.as_str(), |wildcard| {
                        pattern.source.get(..wildcard).unwrap_or_default()
                    });
                if prefix.len() == pattern.source.len() {
                    prefix.starts_with(&directory)
                } else {
                    directory.starts_with(prefix) || prefix.starts_with(&directory)
                }
            })
    }
}

/// `patternmatcher`'s `compile`: `*` and `?` stop at `/`, `**` spans
/// directories, and `\` escapes the next character. As in moby, `^` is not
/// escaped, so it anchors inside a wildcard pattern. `None` is invalid syntax.
fn compile(pattern: &str) -> Option<Kind> {
    #[derive(PartialEq)]
    enum Detected {
        Exact,
        Prefix,
        Suffix,
        Regex,
    }
    let mut regex = String::from("^");
    let mut detected = Detected::Exact;
    let mut chars = pattern.chars().peekable();
    let mut first = true;
    while let Some(ch) = chars.next() {
        match ch {
            '*' if chars.peek() == Some(&'*') => {
                chars.next();
                if chars.peek() == Some(&'/') {
                    chars.next();
                }
                if chars.peek().is_some() {
                    regex.push_str("(.*/)?");
                    detected = Detected::Regex;
                } else if detected == Detected::Exact {
                    detected = Detected::Prefix;
                } else {
                    regex.push_str(".*");
                    detected = Detected::Regex;
                }
                if first {
                    detected = Detected::Suffix;
                }
            }
            '*' => {
                regex.push_str("[^/]*");
                detected = Detected::Regex;
            }
            '?' => {
                regex.push_str("[^/]");
                detected = Detected::Regex;
            }
            '.' | '+' | '(' | ')' | '|' | '{' | '}' | '$' => {
                regex.push('\\');
                regex.push(ch);
            }
            '\\' => {
                // Go rejects a trailing escape as a syntax error.
                let next = chars.next()?;
                regex.push_str(&regex::escape(&next.to_string()));
                detected = Detected::Regex;
            }
            '[' | ']' => {
                regex.push(ch);
                detected = Detected::Regex;
            }
            _ => regex.push(ch),
        }
        first = false;
    }
    Some(match detected {
        Detected::Exact => Kind::Exact,
        Detected::Prefix => Kind::Prefix(pattern.strip_suffix("**")?.to_owned()),
        Detected::Suffix => Kind::Suffix(pattern.get(2..)?.to_owned()),
        Detected::Regex => {
            regex.push('$');
            Kind::Regex(Regex::new(&regex).ok()?)
        }
    })
}

/// The first of the characters at the end of plane 16 that stand for filename
/// bytes that are not UTF-8. Only bytes 0x80-0xFF occur, so the characters
/// U+10FF80-U+10FFFF are reserved: a name or pattern really containing one is
/// refused rather than confused with a raw byte.
const RAW_BYTE: u32 = 0x10_FF00;

const RESERVED: &str = "a name or ignore rule uses a character in U+10FF80-U+10FFFF, which Ployz reserves for raw filename bytes; rename or remove it";

/// Whether valid UTF-8 in `bytes` contains a character reserved for raw bytes.
fn reserved(bytes: &[u8]) -> bool {
    let range = RAW_BYTE + 0x80..=RAW_BYTE + 0xFF;
    bytes
        .utf8_chunks()
        .flat_map(|chunk| chunk.valid().chars())
        .any(|ch| range.contains(&u32::from(ch)))
}

/// Filenames are bytes. Each byte that is not UTF-8 becomes its own character,
/// so distinct names stay distinct and wildcards count it as one character, as
/// Go does.
fn decode(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len());
    for chunk in bytes.utf8_chunks() {
        text.push_str(chunk.valid());
        text.extend(chunk.invalid().iter().map(|byte| {
            char::from_u32(RAW_BYTE + u32::from(*byte)).expect("a plane-16 character")
        }));
    }
    text
}

fn decode_path(path: &Path) -> String {
    decode(path.as_os_str().as_bytes())
}

/// Go's `path.Clean`: the shortest lexically equivalent slash path.
fn clean(path: &str) -> String {
    let rooted = path.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." if parts.last().is_some_and(|last| *last != "..") => {
                parts.pop();
            }
            ".." if rooted => {}
            part => parts.push(part),
        }
    }
    let joined = parts.join("/");
    match (rooted, joined.is_empty()) {
        (true, _) => format!("/{joined}"),
        (false, true) => ".".into(),
        (false, false) => joined,
    }
}

/// Strip comments and trailing commas, as `hujson.Standardize` does.
fn standard_json(text: &str) -> Option<String> {
    let mut json = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '"' => {
                json.push(ch);
                while let Some(ch) = chars.next() {
                    json.push(ch);
                    match ch {
                        '\\' => json.push(chars.next()?),
                        '"' => break,
                        _ => {}
                    }
                }
            }
            '/' if chars.peek() == Some(&'/') => while chars.next_if(|ch| *ch != '\n').is_some() {},
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut previous = None;
                loop {
                    let ch = chars.next()?;
                    if previous == Some('*') && ch == '/' {
                        break;
                    }
                    previous = Some(ch);
                }
            }
            '}' | ']' => {
                let end = json.trim_end().len();
                if json.get(..end).is_some_and(|kept| kept.ends_with(',')) {
                    json.truncate(end - 1);
                }
                json.push(ch);
            }
            _ => json.push(ch),
        }
    }
    Some(json)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matcher(patterns: &[&str]) -> Matcher {
        Matcher::new(&patterns.iter().map(ToString::to_string).collect::<Vec<_>>()).unwrap()
    }

    #[test]
    fn patterns_follow_docker_matching() {
        for (patterns, path, expected) in [
            (&["*.log"][..], "debug.log", true),
            (&["*.log"], "logs/debug.log", false),
            (&["**/*.log"], "logs/deep/debug.log", true),
            (&["logs/**"], "logs/debug.log", true),
            (&["logs/**"], "logs", false),
            (&["cache"], "cache/nested/file", true),
            (&["cache", "!cache/keep"], "cache/keep", false),
            (&["cache", "!cache/keep"], "cache/drop", true),
            (&["cache", "!**/keep"], "cache/keep", false),
            (&["a?c"], "abc", true),
            (&["a?c"], "a/c", false),
            (&["[a-c]x"], "bx", true),
            (&["file.txt"], "fileXtxt", false),
            (&["\\*"], "*", true),
            (&["\\*"], "a", false),
            // Wildcard-free patterns compare as strings; in a regex `^` anchors.
            (&["secret^key"], "secret^key", true),
            (&["**/secret^key"], "a/secret^key", true),
            (&["*^key"], "secret^key", false),
            (&["*^key"], "key", true),
            (&["[^a]x"], "bx", true),
            (&["[^a]x"], "ax", false),
            (&["**/keep"], "keep", true),
        ] {
            assert_eq!(
                matcher(patterns).matches_or_parent_matches(Path::new(path)),
                expected,
                "{patterns:?} {path}"
            );
        }
        assert!(Matcher::new(&["[".into()]).is_err());
        assert!(Matcher::new(&["!".into()]).is_err());
        assert!(Matcher::new(&["foo\\".into()]).is_err());
    }

    #[test]
    fn wildcards_count_characters_and_raw_bytes_stay_distinct() {
        use std::ffi::OsStr;
        let path = |bytes: &[u8]| PathBuf::from(OsStr::from_bytes(bytes));
        // `?` is one character: a multi-byte character is never split.
        assert!(matcher(&["a?"]).matches_or_parent_matches(Path::new("aé")));
        assert!(!matcher(&["a??"]).matches_or_parent_matches(Path::new("aé")));
        // A byte that is not UTF-8 is one character, and distinct bytes stay
        // distinct, so a negation cannot re-include a lookalike sibling.
        let negated = Matcher::new(&["*".into(), decode(b"!a\xff")]).unwrap();
        assert!(!negated.matches_or_parent_matches(&path(b"a\xff")));
        assert!(negated.matches_or_parent_matches(&path(b"a\xfe")));
        assert!(matcher(&["[^b]x"]).matches_or_parent_matches(&path(b"\xffx")));
        assert!(matcher(&["a?"]).matches_or_parent_matches(&path(b"a\xff")));
    }

    #[test]
    fn reserved_characters_are_refused() {
        // A real character that the raw-byte mapping reuses is refused, so it
        // can never be confused with a raw-byte sibling.
        assert!(reserved("a\u{10ff80}".as_bytes()));
        assert!(!reserved(b"a\x80"));
        assert!(!reserved("a\u{10ff7f}".as_bytes()));
        let context = tempfile::tempdir().unwrap();
        fs::write(context.path().join("a\u{10ff80}"), "").unwrap();
        let error = select(context.path(), &Rules::Railpack(None))
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("reserves"), "{error}");
        fs::remove_file(context.path().join("a\u{10ff80}")).unwrap();
        for (file, content) in [
            (".dockerignore", "a\u{10ff80}\n"),
            ("railpack.json", "{\"exclude\": [\"a\u{10ff80}\"]}"),
        ] {
            fs::write(context.path().join(file), content).unwrap();
            let error = select(context.path(), &Rules::Railpack(None))
                .err()
                .unwrap()
                .to_string();
            assert!(error.contains("reserves"), "{file}: {error}");
            fs::remove_file(context.path().join(file)).unwrap();
        }
    }

    #[test]
    fn ignore_files_are_read_like_docker() {
        assert_eq!(
            ignore_file_patterns(
                "\u{feff}# comment\n /cache/ \n!**/keep\n\n ./a/../b\ncache\u{a0}\n".as_bytes()
            ),
            ["cache", "!**/keep", "b", "cache"]
        );
        assert_eq!(clean("a//b/./c/.."), "a/b");
        assert_eq!(clean("../a"), "../a");
        assert_eq!(clean("/../a/"), "/a");
    }

    #[test]
    fn only_exceptions_below_an_ignored_directory_keep_it_walked() {
        let keep = matcher(&["cache", "!cache/keep"]);
        assert!(keep.exception_can_match_descendant(Path::new("cache")));
        assert!(!keep.exception_can_match_descendant(Path::new("other")));
        let wildcard = matcher(&["cache", "!**/keep"]);
        assert!(wildcard.exception_can_match_descendant(Path::new("other")));
    }

    #[test]
    fn railpack_config_must_stay_local_once_cleaned() {
        let context = tempfile::tempdir().unwrap();
        let context = context.path().canonicalize().unwrap();
        fs::write(context.join("railpack.json"), "{}").unwrap();
        for (config, local) in [
            ("config/../railpack.json", true),
            ("./railpack.json", true),
            ("../railpack.json", false),
            ("config/../../railpack.json", false),
            ("/railpack.json", false),
        ] {
            assert_eq!(
                railpack_rules(&context, Some(Path::new(config))).is_ok(),
                local,
                "{config}"
            );
        }
    }

    #[test]
    fn railpack_config_accepts_comments_and_trailing_commas() {
        let json = standard_json(
            "{\n  // comment\n  \"exclude\": [\"a//b\", \"/*c*/\",], /* block */\n}\n",
        )
        .unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value, serde_json::json!({"exclude": ["a//b", "/*c*/"]}));
        assert!(standard_json("{\"a\": 1 /* open").is_none());
    }
}
