//! Fails when test code waits on the real clock.
//!
//! A test that sleeps races the scheduler: too short flakes, too long slows every run.
//! Await the event that marks the milestone (a channel, `Notify`, `watch`, a log line), or
//! run on paused time (`#[tokio::test(start_paused = true)]`, `tokio::time::advance`).
//!
//! Flags `sleep` and `sleep_until` calls in test code: `#[cfg(test)]` modules, `tests/`
//! targets, `#[test]` functions, and the `*-testkit` crates. A `tokio::time` sleep is
//! virtual, and allowed, inside a `start_paused` test, a function that calls
//! `tokio::time::pause()`, or a helper whose callers in the same file are all of those.
//! `std::thread::sleep` always blocks for real.
//!
//! A wait on the OS with nothing to await (a process exit, a file another process writes)
//! carries `// real-wait: <reason>` on its line or in the comment block above it.
//!
//! Usage: `real-wait-lint <crate dir>...`

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::{env, fs};

use proc_macro2::{Span, TokenStream, TokenTree};
use syn::spanned::Spanned;
use syn::visit::Visit;

const MARKER: &str = "real-wait:";

fn main() -> ExitCode {
    let mut findings = Vec::new();
    for dir in env::args().skip(1).map(PathBuf::from) {
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
    let all_test = dir
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with("-testkit"));
    let mut roots = Vec::new();
    for root in ["src/lib.rs", "src/main.rs"] {
        roots.push((dir.join(root), all_test));
    }
    roots.extend(
        rs_files(&dir.join("src/bin"))
            .into_iter()
            .map(|file| (file, all_test)),
    );
    let tests = dir.join("tests");
    roots.extend(rs_files(&tests).into_iter().map(|file| (file, true)));
    if let Ok(entries) = fs::read_dir(&tests) {
        for entry in entries.flatten() {
            let main = entry.path().join("main.rs");
            if main.is_file() {
                roots.push((main, true));
            }
        }
    }
    let mut seen = HashSet::new();
    for (root, in_test) in roots {
        if root.is_file() {
            let dir = root.parent().map(Path::to_path_buf).unwrap_or_default();
            check_file(&root, &dir, in_test, &mut seen, findings)?;
        }
    }
    Ok(())
}

fn rs_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "rs"))
        .collect()
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
}

struct TestFn {
    name: String,
    is_test: bool,
    paused: bool,
    callees: HashSet<String>,
    sleeps: Vec<Sleep>,
}

fn analyze(source: &str, file: &syn::File, in_test: bool) -> Analysis {
    let mut items = Items {
        thread_sleep_imported: imports_thread_sleep(&file.items),
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
            let real = sleep.clock == Clock::Thread || !paused.contains(test_fn.name.as_str());
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
    thread_sleep_imported: bool,
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
        let mut body = Body {
            thread_sleep_imported: self.thread_sleep_imported,
            calls_pause: false,
            callees: HashSet::new(),
            sleeps: Vec::new(),
        };
        body.visit_block(block);
        let start_paused = test_attr.is_some_and(|attr| match &attr.meta {
            syn::Meta::List(list) => compact(list.tokens.clone()).contains("start_paused=true"),
            syn::Meta::Path(_) | syn::Meta::NameValue(_) => false,
        });
        self.fns.push(TestFn {
            name: sig.ident.to_string(),
            is_test: test_attr.is_some(),
            paused: start_paused || body.calls_pause,
            callees: body.callees,
            sleeps: body.sleeps,
        });
    }
}

struct Body {
    thread_sleep_imported: bool,
    calls_pause: bool,
    callees: HashSet<String>,
    sleeps: Vec<Sleep>,
}

impl Body {
    fn call(&mut self, segments: &[String], span: Span) {
        let Some(last) = segments.last() else { return };
        match last.as_str() {
            "sleep" | "sleep_until" => {
                let thread = segments.iter().any(|seg| seg == "thread")
                    || (segments.len() == 1 && self.thread_sleep_imported);
                let clock = if thread { Clock::Thread } else { Clock::Tokio };
                self.sleeps.push(Sleep {
                    line: span.start().line,
                    clock,
                });
            }
            "pause" if segments.len() == 1 || segments.iter().any(|seg| seg == "time") => {
                self.calls_pause = true
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
                        self.call(&path, last_span);
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

impl<'ast> Visit<'ast> for Body {
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
            self.call(&segments, span);
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

fn imports_thread_sleep(items: &[syn::Item]) -> bool {
    fn tree(use_tree: &syn::UseTree, under_thread: bool) -> bool {
        match use_tree {
            syn::UseTree::Path(path) => tree(&path.tree, under_thread || path.ident == "thread"),
            syn::UseTree::Name(name) => under_thread && name.ident == "sleep",
            syn::UseTree::Rename(rename) => under_thread && rename.ident == "sleep",
            syn::UseTree::Group(group) => group.items.iter().any(|item| tree(item, under_thread)),
            syn::UseTree::Glob(_) => false,
        }
    }
    items.iter().any(|item| match item {
        syn::Item::Use(item_use) => tree(&item_use.tree, false),
        syn::Item::Mod(module) => module
            .content
            .as_ref()
            .is_some_and(|(_, items)| imports_thread_sleep(items)),
        _ => false,
    })
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
