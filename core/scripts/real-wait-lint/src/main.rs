//! Fails when test code waits on the real clock.
//!
//! A test that sleeps races the scheduler: too short flakes, too long slows every run.
//! Await the event that marks the milestone (a channel, `Notify`, `watch`, a log line), or
//! run on paused time (`#[tokio::test(start_paused = true)]`, `tokio::time::advance`).
//!
//! Flags `sleep` and `sleep_until` calls in test code: `#[cfg(test)]` modules, `tests/`
//! targets, `#[test]` functions, and the `*-testkit` crates. A `tokio::time` sleep is
//! virtual, and allowed, inside a `start_paused` test, after a `tokio::time::pause()` with
//! no `resume()` between, or in a helper whose callers in the same file are all paused.
//! `std::thread::sleep` always blocks for real.
//!
//! A wait on the OS with nothing to await (a process exit, a file another process writes)
//! carries `// real-wait: <reason>` on its line or in the comment block above it.
//!
//! Usage: `real-wait-lint <crate dir>...`

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::{env, fs};

use proc_macro2::{Span, TokenStream, TokenTree};
use syn::spanned::Spanned;
use syn::visit::Visit;

const MARKER: &str = "real-wait:";

fn main() -> ExitCode {
    let dirs: Vec<PathBuf> = env::args().skip(1).map(PathBuf::from).collect();
    if dirs.is_empty() {
        eprintln!("real-wait-lint: name at least one crate directory");
        return ExitCode::from(2);
    }
    let mut findings = Vec::new();
    for dir in dirs {
        if let Err(error) = check_crate(&dir, &mut findings) {
            eprintln!("real-wait-lint: {error}");
            return ExitCode::from(2);
        }
    }
    findings.sort();
    for finding in &findings {
        println!("{finding}");
    }
    if findings.is_empty() {
        ExitCode::SUCCESS
    } else {
        eprintln!(
            "\n{} real-clock wait(s) in test code. Await the event that marks the milestone, run on \
             paused time (#[tokio::test(start_paused = true)]), or, for an OS wait with nothing to \
             await, add `// {MARKER} <reason>`.",
            findings.len()
        );
        ExitCode::FAILURE
    }
}

fn check_crate(dir: &Path, findings: &mut Vec<String>) -> Result<(), String> {
    if !dir.join("Cargo.toml").is_file() {
        return Err(format!("{} is not a crate directory", dir.display()));
    }
    let all_test = dir
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with("-testkit"));
    let mut roots = Vec::new();
    for root in ["src/lib.rs", "src/main.rs"] {
        roots.push((dir.join(root), all_test));
    }
    for file in entries(&dir.join("src/bin"))? {
        if file.extension().is_some_and(|ext| ext == "rs") {
            roots.push((file, all_test));
        }
    }
    for entry in entries(&dir.join("tests"))? {
        if entry.extension().is_some_and(|ext| ext == "rs") {
            roots.push((entry, true));
        } else if entry.join("main.rs").is_file() {
            roots.push((entry.join("main.rs"), true));
        }
    }
    let mut seen = HashSet::new();
    for (root, in_test) in roots {
        if root.is_file() {
            let dir = root.parent().map(Path::to_path_buf).unwrap_or_default();
            check_file(&root, &dir, in_test, &mut seen, findings)?;
        }
    }
    if seen.is_empty() {
        return Err(format!("{} has no Rust source to scan", dir.display()));
    }
    Ok(())
}

/// The paths in `dir`; none when it does not exist.
fn entries(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let error = |error: std::io::Error| format!("{}: {error}", dir.display());
    match fs::read_dir(dir) {
        Ok(entries) => entries
            .map(|entry| entry.map(|entry| entry.path()).map_err(error))
            .collect(),
        Err(missing) if missing.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(other) => Err(error(other)),
    }
}

/// `mod_dir` is where this file's `mod x;` declarations look for `x.rs`.
fn check_file(
    path: &Path,
    mod_dir: &Path,
    in_test: bool,
    seen: &mut HashSet<(PathBuf, bool)>,
    findings: &mut Vec<String>,
) -> Result<(), String> {
    if !seen.insert((path.to_path_buf(), in_test)) {
        return Ok(());
    }
    let source =
        fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))?;
    let file = syn::parse_file(&source).map_err(|error| format!("{}: {error}", path.display()))?;
    let analysis = analyze(&source, &file, in_test);
    findings.extend(
        analysis
            .findings
            .iter()
            .map(|f| format!("{}:{f}", path.display())),
    );
    let file_dir = path.parent().unwrap_or(Path::new(""));
    for decl in analysis.mods {
        let inline_dir = decl
            .inline_path
            .iter()
            .fold(mod_dir.to_path_buf(), |dir, part| dir.join(part));
        let candidates = match &decl.path_attr {
            Some(attr) if decl.inline_path.is_empty() => vec![file_dir.join(attr)],
            Some(attr) => vec![inline_dir.join(attr)],
            None => vec![
                inline_dir.join(format!("{}.rs", decl.name)),
                inline_dir.join(&decl.name).join("mod.rs"),
            ],
        };
        let Some(child) = candidates.into_iter().find(|candidate| candidate.is_file()) else {
            return Err(format!(
                "{}: cannot find the file for `mod {};`",
                path.display(),
                decl.name
            ));
        };
        let child_parent = child.parent().unwrap_or(Path::new("")).to_path_buf();
        let child_dir =
            if child.file_name().is_some_and(|name| name == "mod.rs") || decl.path_attr.is_some() {
                child_parent
            } else {
                child_parent.join(&decl.name)
            };
        check_file(
            &normalize(&child),
            &normalize(&child_dir),
            decl.in_test,
            seen,
            findings,
        )?;
    }
    Ok(())
}

/// Folds `..` so a file reached through `#[path = "../x.rs"]` dedups and prints as one path.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            std::path::Component::ParentDir if out.file_name().is_some() => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

struct Analysis {
    /// `line: message`, without the file path.
    findings: Vec<String>,
    mods: Vec<ModDecl>,
}

struct ModDecl {
    name: String,
    path_attr: Option<String>,
    /// Inline `mod a { mod b { ... } }` names between the file and this declaration.
    inline_path: Vec<String>,
    in_test: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum Clock {
    /// `std::thread::sleep`: blocks the thread whatever tokio's clock says.
    Thread,
    /// A `tokio::time` sleep: virtual when the runtime's clock is paused.
    Tokio,
}

struct Sleep {
    line: usize,
    clock: Clock,
    /// The function's clock was paused here: `start_paused`, or a `pause()` above it with
    /// no `resume()` between.
    paused: bool,
}

struct TestFn {
    name: String,
    is_test: bool,
    /// Ends on paused time, so a helper it calls may count as paused.
    paused: bool,
    resumes: bool,
    callees: HashSet<String>,
    sleeps: Vec<Sleep>,
}

fn analyze(source: &str, file: &syn::File, in_test: bool) -> Analysis {
    let mut items = Items {
        imported_sleeps: imported_sleeps(&file.items),
        fns: Vec::new(),
        mods: Vec::new(),
        inline_path: Vec::new(),
    };
    items.walk(&file.items, in_test);
    let fns = items.fns;

    let mut paused: HashSet<&str> = fns
        .iter()
        .filter(|f| f.paused)
        .map(|f| f.name.as_str())
        .collect();
    loop {
        let newly_paused: Vec<&str> = fns
            .iter()
            .filter(|helper| !helper.is_test && !paused.contains(helper.name.as_str()))
            .filter(|helper| {
                let mut callers = fns
                    .iter()
                    .filter(|f| f.name != helper.name && f.callees.contains(&helper.name))
                    .peekable();
                callers.peek().is_some() && callers.all(|f| paused.contains(f.name.as_str()))
            })
            .map(|helper| helper.name.as_str())
            .collect();
        if newly_paused.is_empty() {
            break;
        }
        paused.extend(newly_paused);
    }

    let lines: Vec<&str> = source.lines().collect();
    let mut findings = Vec::new();
    for test_fn in &fns {
        for sleep in &test_fn.sleeps {
            let helper_paused = !test_fn.resumes && paused.contains(test_fn.name.as_str());
            let real = sleep.clock == Clock::Thread || !(sleep.paused || helper_paused);
            match (real, marked(&lines, sleep.line)) {
                (true, Some(true)) | (false, None) => {}
                (true, None) => {
                    findings.push(format!("{}: real-clock sleep in test code", sleep.line))
                }
                (true, Some(false)) => {
                    findings.push(format!("{}: `// {MARKER}` needs a reason", sleep.line))
                }
                (false, Some(_)) => findings.push(format!(
                    "{}: `// {MARKER}` on a sleep that runs on paused time; remove it",
                    sleep.line
                )),
            }
        }
    }
    Analysis {
        findings,
        mods: items.mods,
    }
}

/// `None` when the line carries no marker, otherwise whether the marker gives a reason.
fn marked(lines: &[&str], line: usize) -> Option<bool> {
    let comment_above = lines
        .get(..line.saturating_sub(1))
        .unwrap_or_default()
        .iter()
        .rev()
        .take_while(|text| text.trim_start().starts_with("//"));
    lines
        .get(line.wrapping_sub(1))
        .into_iter()
        .chain(comment_above)
        .find_map(|text| {
            let (_, after) = text.split_once("//")?;
            let reason = after.trim_start().strip_prefix(MARKER)?;
            Some(!reason.trim().is_empty())
        })
}

struct Items {
    /// The clock of each sleep a `use` brings in, by the name it is called under.
    imported_sleeps: HashMap<String, Clock>,
    fns: Vec<TestFn>,
    mods: Vec<ModDecl>,
    inline_path: Vec<String>,
}

impl Items {
    fn walk(&mut self, items: &[syn::Item], in_test: bool) {
        for item in items {
            match item {
                syn::Item::Mod(module) => {
                    let in_test = in_test || is_cfg_test(&module.attrs);
                    let name = module.ident.to_string();
                    if let Some((_, content)) = &module.content {
                        self.inline_path.push(name);
                        self.walk(content, in_test);
                        self.inline_path.pop();
                    } else {
                        self.mods.push(ModDecl {
                            name,
                            path_attr: path_attr(&module.attrs),
                            inline_path: self.inline_path.clone(),
                            in_test,
                        });
                    }
                }
                syn::Item::Fn(item_fn) => {
                    self.function(&item_fn.sig, &item_fn.attrs, &item_fn.block, in_test)
                }
                syn::Item::Impl(item_impl) => {
                    let in_test = in_test || is_cfg_test(&item_impl.attrs);
                    for impl_item in &item_impl.items {
                        if let syn::ImplItem::Fn(method) = impl_item {
                            self.function(&method.sig, &method.attrs, &method.block, in_test);
                        }
                    }
                }
                syn::Item::Trait(item_trait) => {
                    for trait_item in &item_trait.items {
                        if let syn::TraitItem::Fn(syn::TraitItemFn {
                            sig,
                            attrs,
                            default: Some(block),
                            ..
                        }) = trait_item
                        {
                            self.function(sig, attrs, block, in_test);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn function(
        &mut self,
        sig: &syn::Signature,
        attrs: &[syn::Attribute],
        block: &syn::Block,
        in_test: bool,
    ) {
        let test_attr = attrs.iter().find(|attr| {
            attr.path()
                .segments
                .last()
                .is_some_and(|seg| seg.ident == "test")
        });
        if !in_test && test_attr.is_none() && !is_cfg_test(attrs) {
            return;
        }
        let start_paused = test_attr.is_some_and(|attr| match &attr.meta {
            syn::Meta::List(list) => compact(list.tokens.clone()).contains("start_paused=true"),
            syn::Meta::Path(_) | syn::Meta::NameValue(_) => false,
        });
        let mut body = Body {
            imported_sleeps: &self.imported_sleeps,
            paused: start_paused,
            resumes: false,
            callees: HashSet::new(),
            sleeps: Vec::new(),
        };
        body.visit_block(block);
        self.fns.push(TestFn {
            name: sig.ident.to_string(),
            is_test: test_attr.is_some(),
            paused: body.paused,
            resumes: body.resumes,
            callees: body.callees,
            sleeps: body.sleeps,
        });
    }
}

struct Body<'a> {
    imported_sleeps: &'a HashMap<String, Clock>,
    /// Whether the clock is paused at the point the visit has reached.
    paused: bool,
    resumes: bool,
    callees: HashSet<String>,
    sleeps: Vec<Sleep>,
}

impl Body<'_> {
    /// `no_args` tells `tokio::time::pause()` from a test's own `pause(duration)`.
    fn call(&mut self, segments: &[String], span: Span, no_args: bool) {
        let Some(last) = segments.last() else { return };
        let clock = match segments {
            [name] => self.imported_sleeps.get(name).copied().or(matches!(
                name.as_str(),
                "sleep" | "sleep_until"
            )
            .then_some(Clock::Tokio)),
            _ if matches!(last.as_str(), "sleep" | "sleep_until") => {
                Some(if segments.iter().any(|seg| seg == "thread") {
                    Clock::Thread
                } else {
                    Clock::Tokio
                })
            }
            _ => None,
        };
        if let Some(clock) = clock {
            self.sleeps.push(Sleep {
                line: span.start().line,
                clock,
                paused: self.paused,
            });
        }
        let tokio_time = segments.len() == 1 || segments.iter().any(|seg| seg == "time");
        match last.as_str() {
            "pause" if no_args && tokio_time => self.paused = true,
            "resume" if no_args && tokio_time => {
                self.paused = false;
                self.resumes = true;
            }
            _ => {}
        }
        self.callees.insert(last.clone());
    }

    /// Macro bodies are tokens to syn, so `select!` and `join!` arms are found by shape:
    /// a path ending in an identifier, followed by a parenthesized group.
    fn tokens(&mut self, tokens: TokenStream) {
        let mut path: Vec<String> = Vec::new();
        let mut last_span = Span::call_site();
        let mut colons = 0;
        for tree in tokens {
            match tree {
                TokenTree::Ident(ident) => {
                    if colons != 2 {
                        path.clear();
                    }
                    colons = 0;
                    last_span = ident.span();
                    path.push(ident.to_string());
                }
                TokenTree::Punct(punct) if punct.as_char() == ':' && !path.is_empty() => {
                    colons += 1
                }
                TokenTree::Group(group) => {
                    if group.delimiter() == proc_macro2::Delimiter::Parenthesis
                        && colons == 0
                        && !path.is_empty()
                    {
                        self.call(&path, last_span, group.stream().is_empty());
                    }
                    path.clear();
                    colons = 0;
                    self.tokens(group.stream());
                }
                TokenTree::Punct(_) | TokenTree::Literal(_) => {
                    path.clear();
                    colons = 0;
                }
            }
        }
    }
}

impl<'ast> Visit<'ast> for Body<'_> {
    fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
        if let syn::Expr::Path(expr_path) = &*call.func {
            let segments: Vec<String> = expr_path
                .path
                .segments
                .iter()
                .map(|seg| seg.ident.to_string())
                .collect();
            let span = expr_path
                .path
                .segments
                .last()
                .map_or_else(|| call.span(), |seg| seg.ident.span());
            syn::visit::visit_expr_call(self, call);
            self.call(&segments, span, call.args.is_empty());
            return;
        }
        syn::visit::visit_expr_call(self, call);
    }

    fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
        self.callees.insert(call.method.to_string());
        syn::visit::visit_expr_method_call(self, call);
    }

    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        self.tokens(mac.tokens.clone());
    }
}

fn imported_sleeps(items: &[syn::Item]) -> HashMap<String, Clock> {
    fn tree(use_tree: &syn::UseTree, under_thread: bool, out: &mut HashMap<String, Clock>) {
        let clock = if under_thread {
            Clock::Thread
        } else {
            Clock::Tokio
        };
        let is_sleep = |ident: &syn::Ident| ident == "sleep" || ident == "sleep_until";
        match use_tree {
            syn::UseTree::Path(path) => {
                tree(&path.tree, under_thread || path.ident == "thread", out)
            }
            syn::UseTree::Name(name) if is_sleep(&name.ident) => {
                out.insert(name.ident.to_string(), clock);
            }
            syn::UseTree::Rename(rename) if is_sleep(&rename.ident) => {
                out.insert(rename.rename.to_string(), clock);
            }
            syn::UseTree::Group(group) => {
                for item in &group.items {
                    tree(item, under_thread, out);
                }
            }
            syn::UseTree::Name(_) | syn::UseTree::Rename(_) | syn::UseTree::Glob(_) => {}
        }
    }
    let mut out = HashMap::new();
    for item in items {
        match item {
            syn::Item::Use(item_use) => tree(&item_use.tree, false, &mut out),
            syn::Item::Mod(syn::ItemMod {
                content: Some((_, items)),
                ..
            }) => out.extend(imported_sleeps(items)),
            _ => {}
        }
    }
    out
}

fn is_cfg_test(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        let syn::Meta::List(list) = &attr.meta else {
            return false;
        };
        let tokens = compact(list.tokens.clone());
        list.path.is_ident("cfg")
            && !tokens.contains("not(test")
            && tokens
                .split(|c: char| !c.is_alphanumeric() && c != '_')
                .any(|word| word == "test")
    })
}

fn path_attr(attrs: &[syn::Attribute]) -> Option<String> {
    attrs.iter().find_map(|attr| {
        let syn::Meta::NameValue(name_value) = &attr.meta else {
            return None;
        };
        if !name_value.path.is_ident("path") {
            return None;
        }
        let syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Str(path),
            ..
        }) = &name_value.value
        else {
            return None;
        };
        Some(path.value())
    })
}

fn compact(tokens: TokenStream) -> String {
    tokens
        .to_string()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lint(source: &str) -> Vec<String> {
        analyze(source, &syn::parse_file(source).unwrap(), false).findings
    }

    #[test]
    fn flags_a_real_sleep_in_a_test() {
        let source = r#"
#[tokio::test]
async fn waits() {
    tokio::time::sleep(Duration::from_millis(50)).await;
}
"#;
        assert_eq!(lint(source), ["4: real-clock sleep in test code"]);
    }

    #[test]
    fn allows_a_tokio_sleep_on_paused_time_and_in_its_helpers() {
        let source = r#"
#[cfg(test)]
mod tests {
    use tokio::time::sleep;

    async fn settle() {
        sleep(Duration::from_secs(5)).await;
    }

    #[tokio::test(start_paused = true)]
    async fn waits() {
        settle().await;
        tokio::select! { _ = tokio::time::sleep(Duration::from_secs(1)) => {} }
    }

    #[tokio::test]
    async fn pauses_itself() {
        tokio::time::pause();
        sleep(Duration::from_secs(1)).await;
    }
}
"#;
        assert!(lint(source).is_empty(), "{:?}", lint(source));
    }

    #[test]
    fn flags_a_helper_with_a_real_clock_caller() {
        let source = r#"
#[cfg(test)]
mod tests {
    async fn settle() {
        tokio::time::sleep(Duration::from_secs(5)).await;
    }

    #[tokio::test(start_paused = true)]
    async fn paused() { settle().await; }

    #[tokio::test]
    async fn real() { settle().await; }
}
"#;
        assert_eq!(lint(source), ["5: real-clock sleep in test code"]);
    }

    #[test]
    fn flags_a_thread_sleep_even_on_paused_time() {
        let source = r#"
use std::thread::sleep;

#[tokio::test(start_paused = true)]
async fn waits() {
    sleep(Duration::from_millis(5));
    std::thread::sleep(Duration::from_millis(5));
}
"#;
        assert_eq!(
            lint(source),
            [
                "6: real-clock sleep in test code",
                "7: real-clock sleep in test code"
            ]
        );
    }

    #[test]
    fn finds_sleeps_inside_macros() {
        let source = r#"
#[test]
fn waits() {
    tokio::select! {
        _ = tokio::time::sleep(Duration::from_millis(5)) => {}
        _ = rx.recv() => {}
    }
    assert!(matches!(other::sleep(1), Ok(())));
}
"#;
        assert_eq!(
            lint(source),
            [
                "5: real-clock sleep in test code",
                "8: real-clock sleep in test code"
            ]
        );
    }

    #[test]
    fn a_marker_with_a_reason_allows_a_real_wait() {
        let source = r#"
#[test]
fn waits() {
    // The child writes its pid file with nothing to await.
    // real-wait: polls a file another process writes
    std::thread::sleep(Duration::from_millis(5));
    std::thread::sleep(Duration::from_millis(5)); // real-wait: same poll
    // real-wait:
    std::thread::sleep(Duration::from_millis(5));
}
"#;
        assert_eq!(lint(source), ["9: `// real-wait:` needs a reason"]);
    }

    #[test]
    fn rejects_a_marker_on_paused_time() {
        let source = r#"
#[tokio::test(start_paused = true)]
async fn waits() {
    // real-wait: not real at all
    tokio::time::sleep(Duration::from_secs(1)).await;
}
"#;
        assert_eq!(
            lint(source),
            ["5: `// real-wait:` on a sleep that runs on paused time; remove it"]
        );
    }

    #[test]
    fn follows_a_renamed_sleep_import() {
        let source = r#"
use std::thread::sleep as wait;
use tokio::time::sleep as nap;

#[tokio::test]
async fn waits() {
    wait(Duration::from_millis(5));
    nap(Duration::from_millis(5)).await;
}
"#;
        assert_eq!(
            lint(source),
            [
                "7: real-clock sleep in test code",
                "8: real-clock sleep in test code"
            ]
        );
    }

    #[test]
    fn a_resume_ends_paused_time() {
        let source = r#"
#[tokio::test]
async fn waits() {
    tokio::time::pause();
    tokio::time::sleep(Duration::from_secs(1)).await;
    tokio::time::resume();
    tokio::time::sleep(Duration::from_secs(1)).await;
}
"#;
        assert_eq!(lint(source), ["7: real-clock sleep in test code"]);
    }

    #[test]
    fn a_pause_with_arguments_is_not_tokios() {
        let source = r#"
#[tokio::test]
async fn waits() {
    let _ = pause(Duration::from_secs(1));
    tokio::time::sleep(Duration::from_secs(1)).await;
}
"#;
        assert_eq!(lint(source), ["5: real-clock sleep in test code"]);
    }

    #[test]
    fn ignores_production_code_and_cfg_not_test() {
        let source = r#"
async fn backoff() { tokio::time::sleep(Duration::from_secs(1)).await; }

#[cfg(not(test))]
mod live {
    fn wait() { std::thread::sleep(Duration::from_secs(1)); }
}
"#;
        assert!(lint(source).is_empty());
    }
}
