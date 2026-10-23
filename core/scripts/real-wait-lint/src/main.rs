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
//! A `pause()` counts only on the paths that run it: one in a branch, a loop, a closure or
//! an async block that isn't awaited in place leaves a sleep after it real.
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

struct Call {
    path: Vec<String>,
    method: bool,
    paused: bool,
    /// Production code never runs a test helper, so only test code decides its clock.
    in_test: bool,
}

struct TestFn {
    name: Vec<String>,
    method: bool,
    is_test: bool,
    /// A future created while paused can be awaited after the caller resumes.
    paused: bool,
    resumes: bool,
    callees: Vec<Call>,
    sleeps: Vec<Sleep>,
}

fn analyze(source: &str, file: &syn::File, in_test: bool) -> Analysis {
    let mut items = Items {
        imported_sleeps: imported_sleeps(&file.items),
        imported_paths: imported_paths(&file.items, &[]),
        fns: Vec::new(),
        mods: Vec::new(),
        inline_path: Vec::new(),
        module_path: Vec::new(),
    };
    items.walk(&file.items, in_test);
    let fns = items.fns;

    let names: HashSet<_> = fns.iter().map(|f| f.name.clone()).collect();
    let mut callers: HashMap<_, Vec<_>> = HashMap::new();
    for f in &fns {
        for call in f.callees.iter().filter(|call| call.in_test) {
            let resolved = if call.method {
                None
            } else {
                resolve_call(&f.name, &call.path, &names)
            };
            let targets: Vec<_> = match resolved {
                Some(name) => vec![name],
                None => fns
                    .iter()
                    .filter(|target| !call.method || target.method)
                    .filter(|target| target.name.last() == call.path.last())
                    .map(|target| target.name.clone())
                    .collect(),
            };
            for name in targets {
                if name != f.name {
                    callers.entry(name).or_default().push((f, call.paused));
                }
            }
        }
    }
    let mut inherited = HashSet::new();
    loop {
        let newly_paused: Vec<_> = fns
            .iter()
            .filter(|helper| !helper.is_test && !inherited.contains(&helper.name))
            .filter(|helper| {
                callers.get(&helper.name).is_some_and(|calls| {
                    calls.iter().all(|(f, paused)| {
                        (*paused && f.paused) || (!f.resumes && inherited.contains(&f.name))
                    })
                })
            })
            .map(|helper| helper.name.clone())
            .collect();
        if newly_paused.is_empty() {
            break;
        }
        inherited.extend(newly_paused);
    }

    let lines: Vec<&str> = source.lines().collect();
    let mut findings = Vec::new();
    for test_fn in &fns {
        for sleep in &test_fn.sleeps {
            let helper_paused = !test_fn.resumes && inherited.contains(&test_fn.name);
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

fn resolve_call(
    caller: &[String],
    path: &[String],
    names: &HashSet<Vec<String>>,
) -> Option<Vec<String>> {
    let mut scope = caller[..caller.len() - 1].to_vec();
    let mut path = path;
    match path.first().map(String::as_str) {
        Some("crate") => {
            scope.clear();
            path = &path[1..];
        }
        Some("self" | "Self") => path = &path[1..],
        Some("super") => {
            while path.first().is_some_and(|part| part == "super") {
                scope.pop();
                path = &path[1..];
            }
        }
        _ => scope = caller.to_vec(),
    }
    loop {
        let name = scope.iter().chain(path).cloned().collect::<Vec<_>>();
        if names.contains(&name) {
            return Some(name);
        }
        scope.pop()?;
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
    imported_paths: HashMap<String, Vec<String>>,
    fns: Vec<TestFn>,
    mods: Vec<ModDecl>,
    inline_path: Vec<String>,
    module_path: Vec<String>,
}

impl Items {
    fn walk(&mut self, items: &[syn::Item], in_test: bool) {
        for item in items {
            match item {
                syn::Item::Mod(module) => {
                    let in_test = in_test || is_cfg_test(&module.attrs);
                    let name = module.ident.to_string();
                    if let Some((_, content)) = &module.content {
                        let outer = self.imported_sleeps.clone();
                        self.imported_sleeps.extend(imported_sleeps(content));
                        let outer_paths = self.imported_paths.clone();
                        self.module_path.push(name.clone());
                        self.inline_path.push(name);
                        self.imported_paths
                            .extend(imported_paths(content, &self.inline_path));
                        self.walk(content, in_test);
                        self.inline_path.pop();
                        self.module_path.pop();
                        self.imported_sleeps = outer;
                        self.imported_paths = outer_paths;
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
                    let owner = match &*item_impl.self_ty {
                        syn::Type::Path(ty) => ty
                            .path
                            .segments
                            .last()
                            .expect("a parsed type path has a segment")
                            .ident
                            .to_string(),
                        _ => format!(
                            "impl@{}:{}",
                            item_impl.span().start().line,
                            item_impl.span().start().column
                        ),
                    };
                    self.inline_path.push(owner);
                    for impl_item in &item_impl.items {
                        if let syn::ImplItem::Fn(method) = impl_item {
                            self.function(&method.sig, &method.attrs, &method.block, in_test);
                        }
                    }
                    self.inline_path.pop();
                }
                syn::Item::Trait(item_trait) => {
                    self.inline_path.push(item_trait.ident.to_string());
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
                    self.inline_path.pop();
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
        let start_paused = test_attr.is_some_and(|attr| match &attr.meta {
            syn::Meta::List(list) => compact(list.tokens.clone()).contains("start_paused=true"),
            syn::Meta::Path(_) | syn::Meta::NameValue(_) => false,
        });
        let name: Vec<_> = self
            .inline_path
            .iter()
            .cloned()
            .chain([sig.ident.to_string()])
            .collect();
        let mut body = Body {
            name: name.clone(),
            nested_fns: Vec::new(),
            imported_sleeps: self.imported_sleeps.clone(),
            imported_paths: self.imported_paths.clone(),
            scope: self.module_path.clone(),
            in_test: in_test || test_attr.is_some() || is_cfg_test(attrs),
            paused: start_paused,
            resumes: false,
            diverged: false,
            loops: Vec::new(),
            returns: None,
            callees: Vec::new(),
            sleeps: Vec::new(),
        };
        body.visit_block(block);
        self.fns.extend(body.nested_fns);
        self.fns.push(TestFn {
            name,
            method: sig.receiver().is_some(),
            is_test: test_attr.is_some(),
            paused: body.paused,
            resumes: body.resumes,
            callees: body.callees,
            sleeps: body.sleeps,
        });
    }
}

/// Where control leaves a loop: its `break`s, and its `continue`s back to the top.
struct Loop {
    label: Option<String>,
    /// A labeled block: only a `break` naming it leaves it.
    block: bool,
    /// Paused at every `break` seen so far; `None` before the first.
    breaks: Option<bool>,
    /// Paused at every `continue` seen so far; `None` before the first.
    continues: Option<bool>,
}

struct Body {
    name: Vec<String>,
    nested_fns: Vec<TestFn>,
    scope: Vec<String>,
    imported_sleeps: HashMap<String, Clock>,
    imported_paths: HashMap<String, Vec<String>>,
    in_test: bool,
    /// Whether the clock is paused on the path being visited. Paths join where control
    /// flow does: after a branch, it is paused only if every branch that falls through
    /// paused it, and after a loop only if every `break` did. A `resume()` anywhere counts
    /// as run.
    paused: bool,
    resumes: bool,
    /// The path being visited has returned, broken out or panicked, so no later code in
    /// its block runs on it.
    diverged: bool,
    /// The loops and labeled blocks around the code being visited, innermost last.
    loops: Vec<Loop>,
    /// Paused at every `return` from an async block awaited in place, while visiting one:
    /// its returns end the block, not the function. `Some(None)` before the first.
    returns: Option<Option<bool>>,
    callees: Vec<Call>,
    sleeps: Vec<Sleep>,
}

impl Body {
    /// Visits code that may not run, or not before what follows it: the right side of
    /// `&&` or `||`, or a macro's tokens. Paused time leaves it only if it was paused on
    /// the way in and nothing inside resumed.
    fn may_not_run(&mut self, visit: impl FnOnce(&mut Self)) {
        let (paused, diverged) = (self.paused, self.diverged);
        visit(self);
        self.paused &= paused;
        self.diverged = diverged;
    }

    /// Visits a closure or an async block that isn't awaited where it's written. It may run
    /// later or never, and its `return`s and `break`s don't leave the code around it.
    fn deferred(&mut self, visit: impl FnOnce(&mut Self)) {
        let loops = std::mem::take(&mut self.loops);
        let returns = self.returns.take();
        self.may_not_run(visit);
        (self.loops, self.returns) = (loops, returns);
    }

    /// Visits one way through a branch, starting from `paused`. `None` when it diverged.
    fn branch(&mut self, paused: bool, visit: impl FnOnce(&mut Self)) -> Option<bool> {
        self.paused = paused;
        self.diverged = false;
        visit(self);
        (!self.diverged).then_some(self.paused)
    }

    /// Joins the ways through a branch: paused if every one that fell through paused, and
    /// diverged if none did.
    fn join(&mut self, paused: bool, ways: impl IntoIterator<Item = Option<bool>>) {
        let mut through = ways.into_iter().flatten().peekable();
        self.diverged = through.peek().is_none();
        self.paused = if self.diverged {
            paused
        } else {
            through.all(|way| way)
        };
    }

    /// Visits a loop. The body runs twice: once to learn what a later iteration starts
    /// with, discarding what it found, then for real from that start. Returns the loop's
    /// breaks and where its body ended, if it fell through.
    fn in_loop(
        &mut self,
        label: Option<&syn::Label>,
        visit: impl Fn(&mut Self),
    ) -> (Option<bool>, Option<bool>) {
        let start = self.paused;
        let found = (self.sleeps.len(), self.callees.len(), self.nested_fns.len());
        let iterate = |body: &mut Self, paused: bool| {
            body.loops.push(Loop {
                label: label.map(|label| label.name.ident.to_string()),
                block: false,
                breaks: None,
                continues: None,
            });
            let end = body.branch(paused, &visit);
            let ended = body.loops.pop().expect("pushed above");
            (ended, end)
        };
        let (first, end) = iterate(self, start);
        self.sleeps.truncate(found.0);
        self.callees.truncate(found.1);
        self.nested_fns.truncate(found.2);
        let later = [Some(start), end, first.continues]
            .into_iter()
            .flatten()
            .all(|way| way);
        let (ended, end) = iterate(self, later);
        (ended.breaks, end)
    }

    /// Records that the innermost loop may end here without a `break`: a `while` whose
    /// condition fails, or a `for` with nothing left.
    fn exit_here(&mut self) {
        let paused = self.paused;
        if let Some(target) = self.loops.last_mut() {
            target.breaks = Some(target.breaks.unwrap_or(true) && paused);
        }
    }

    /// Records a `break` or `continue` from here to the loop it names.
    fn leave(&mut self, label: Option<&syn::Lifetime>, continues: bool) {
        let paused = self.paused;
        let target = match label {
            Some(label) => {
                let name = label.ident.to_string();
                self.loops
                    .iter_mut()
                    .rev()
                    .find(|target| target.label.as_deref() == Some(name.as_str()))
            }
            None => self.loops.iter_mut().rev().find(|target| !target.block),
        };
        if let Some(target) = target {
            let ways = if continues {
                &mut target.continues
            } else {
                &mut target.breaks
            };
            *ways = Some(ways.unwrap_or(true) && paused);
        }
        self.diverged = true;
    }

    /// `no_args` tells `tokio::time::pause()` from a test's own `pause(duration)`.
    fn call(&mut self, segments: &[String], span: Span, no_args: bool) {
        let imported_clock = match segments {
            [name] => self.imported_sleeps.get(name).copied(),
            _ => None,
        };
        let expanded = self.expand_path(segments);
        let segments = expanded.as_slice();
        let Some(last) = segments.last() else { return };
        let clock = imported_clock.or_else(|| match segments {
            [name] => matches!(name.as_str(), "sleep" | "sleep_until").then_some(Clock::Tokio),
            _ if matches!(last.as_str(), "sleep" | "sleep_until") => {
                Some(if segments.iter().any(|seg| seg == "thread") {
                    Clock::Thread
                } else {
                    Clock::Tokio
                })
            }
            _ => None,
        });
        if let Some(clock) = clock.filter(|_| self.in_test) {
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
        self.callees.push(Call {
            path: expanded,
            method: false,
            paused: self.paused,
            in_test: self.in_test,
        });
    }

    fn expand_path(&self, path: &[String]) -> Vec<String> {
        match path
            .first()
            .and_then(|first| self.imported_paths.get(first))
        {
            Some(imported) => imported.iter().chain(&path[1..]).cloned().collect(),
            None => path.to_vec(),
        }
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

impl<'ast> Visit<'ast> for Body {
    fn visit_attribute(&mut self, attr: &'ast syn::Attribute) {
        self.in_test |= is_cfg_test(std::slice::from_ref(attr));
    }

    fn visit_stmt(&mut self, stmt: &'ast syn::Stmt) {
        let in_test = self.in_test;
        syn::visit::visit_stmt(self, stmt);
        self.in_test = in_test;
    }

    fn visit_local_init(&mut self, init: &'ast syn::LocalInit) {
        self.visit_expr(&init.expr);
        let paused = self.paused;
        if let Some((_, diverge)) = &init.diverge {
            self.visit_expr(diverge);
        }
        // The `else` diverges, so the code after the `let` runs only on the other path.
        self.paused = paused;
        self.diverged = false;
    }

    fn visit_arm(&mut self, arm: &'ast syn::Arm) {
        let in_test = self.in_test;
        syn::visit::visit_arm(self, arm);
        self.in_test = in_test;
    }

    fn visit_expr_if(&mut self, expr: &'ast syn::ExprIf) {
        for attr in &expr.attrs {
            self.visit_attribute(attr);
        }
        self.visit_expr(&expr.cond);
        let start = self.paused;
        let then = self.branch(start, |body| body.visit_block(&expr.then_branch));
        let otherwise = match &expr.else_branch {
            Some((_, otherwise)) => self.branch(start, |body| body.visit_expr(otherwise)),
            None => Some(start),
        };
        self.join(start, [then, otherwise]);
    }

    fn visit_expr_match(&mut self, expr: &'ast syn::ExprMatch) {
        for attr in &expr.attrs {
            self.visit_attribute(attr);
        }
        self.visit_expr(&expr.expr);
        let start = self.paused;
        // A failed guard has run before the next arm is tried.
        let mut next = start;
        let mut ways = Vec::new();
        for arm in &expr.arms {
            let mut tried = next;
            ways.push(self.branch(next, |body| {
                let in_test = body.in_test;
                for attr in &arm.attrs {
                    body.visit_attribute(attr);
                }
                body.visit_pat(&arm.pat);
                if let Some((_, guard)) = &arm.guard {
                    body.visit_expr(guard);
                    tried = body.paused;
                }
                body.visit_expr(&arm.body);
                body.in_test = in_test;
            }));
            next = next && tried;
        }
        self.join(start, ways);
    }

    fn visit_expr_binary(&mut self, expr: &'ast syn::ExprBinary) {
        for attr in &expr.attrs {
            self.visit_attribute(attr);
        }
        self.visit_expr(&expr.left);
        if matches!(expr.op, syn::BinOp::And(_) | syn::BinOp::Or(_)) {
            self.may_not_run(|body| body.visit_expr(&expr.right));
        } else {
            self.visit_expr(&expr.right);
        }
    }

    fn visit_expr_loop(&mut self, expr: &'ast syn::ExprLoop) {
        for attr in &expr.attrs {
            self.visit_attribute(attr);
        }
        let (breaks, _) = self.in_loop(expr.label.as_ref(), |body| body.visit_block(&expr.body));
        // A loop no `break` leaves runs forever or returns, so nothing after it runs.
        self.diverged = breaks.is_none();
        self.paused = breaks.unwrap_or(self.paused);
    }

    fn visit_expr_while(&mut self, expr: &'ast syn::ExprWhile) {
        for attr in &expr.attrs {
            self.visit_attribute(attr);
        }
        let (breaks, _) = self.in_loop(expr.label.as_ref(), |body| {
            body.visit_expr(&expr.cond);
            body.exit_here();
            body.visit_block(&expr.body);
        });
        self.diverged = false;
        self.paused = breaks.unwrap_or(self.paused);
    }

    fn visit_expr_for_loop(&mut self, expr: &'ast syn::ExprForLoop) {
        for attr in &expr.attrs {
            self.visit_attribute(attr);
        }
        self.visit_expr(&expr.expr);
        let (breaks, _) = self.in_loop(expr.label.as_ref(), |body| {
            body.exit_here();
            body.visit_pat(&expr.pat);
            body.visit_block(&expr.body);
        });
        self.diverged = false;
        self.paused = breaks.unwrap_or(self.paused);
    }

    fn visit_expr_break(&mut self, expr: &'ast syn::ExprBreak) {
        syn::visit::visit_expr_break(self, expr);
        self.leave(expr.label.as_ref(), false);
    }

    fn visit_expr_continue(&mut self, expr: &'ast syn::ExprContinue) {
        syn::visit::visit_expr_continue(self, expr);
        self.leave(expr.label.as_ref(), true);
    }

    fn visit_expr_return(&mut self, expr: &'ast syn::ExprReturn) {
        syn::visit::visit_expr_return(self, expr);
        if let Some(returns) = &mut self.returns {
            *returns = Some(returns.unwrap_or(true) && self.paused);
        }
        self.diverged = true;
    }

    /// A labeled block ends where it falls through, or at a `break` naming it.
    fn visit_expr_block(&mut self, expr: &'ast syn::ExprBlock) {
        let Some(label) = &expr.label else {
            syn::visit::visit_expr_block(self, expr);
            return;
        };
        for attr in &expr.attrs {
            self.visit_attribute(attr);
        }
        let start = self.paused;
        self.loops.push(Loop {
            label: Some(label.name.ident.to_string()),
            block: true,
            breaks: None,
            continues: None,
        });
        let end = self.branch(start, |body| body.visit_block(&expr.block));
        let breaks = self.loops.pop().expect("pushed above").breaks;
        self.join(start, [end, breaks]);
    }

    fn visit_expr_closure(&mut self, expr: &'ast syn::ExprClosure) {
        self.deferred(|body| syn::visit::visit_expr_closure(body, expr));
    }

    fn visit_expr_async(&mut self, expr: &'ast syn::ExprAsync) {
        self.deferred(|body| syn::visit::visit_expr_async(body, expr));
    }

    /// An async block awaited where it's written runs there, like a block.
    fn visit_expr_await(&mut self, expr: &'ast syn::ExprAwait) {
        let mut base = &*expr.base;
        while let syn::Expr::Paren(syn::ExprParen { expr: inner, .. }) = base {
            base = inner;
        }
        let syn::Expr::Async(block) = base else {
            syn::visit::visit_expr_await(self, expr);
            return;
        };
        for attr in expr.attrs.iter().chain(&block.attrs) {
            self.visit_attribute(attr);
        }
        let loops = std::mem::take(&mut self.loops);
        let returns = self.returns.replace(None);
        let start = self.paused;
        let end = self.branch(start, |body| body.visit_block(&block.block));
        // A `return` in it ends the block, not the function.
        let exits = self.returns.take().flatten();
        (self.loops, self.returns) = (loops, returns);
        self.join(start, [end, exits]);
    }

    fn visit_field_value(&mut self, field: &'ast syn::FieldValue) {
        let in_test = self.in_test;
        syn::visit::visit_field_value(self, field);
        self.in_test = in_test;
    }

    fn visit_expr(&mut self, expr: &'ast syn::Expr) {
        let in_test = self.in_test;
        syn::visit::visit_expr(self, expr);
        self.in_test = in_test;
    }

    fn visit_item_fn(&mut self, item: &'ast syn::ItemFn) {
        let mut items = Items {
            imported_sleeps: self.imported_sleeps.clone(),
            imported_paths: self.imported_paths.clone(),
            fns: Vec::new(),
            mods: Vec::new(),
            inline_path: self.name.clone(),
            module_path: self.scope.clone(),
        };
        items.function(&item.sig, &item.attrs, &item.block, self.in_test);
        self.nested_fns.extend(items.fns);
    }

    fn visit_item(&mut self, item: &'ast syn::Item) {
        let (paused, diverged) = (self.paused, self.diverged);
        syn::visit::visit_item(self, item);
        (self.paused, self.diverged) = (paused, diverged);
    }

    fn visit_block(&mut self, block: &'ast syn::Block) {
        let outer = self.imported_sleeps.clone();
        let outer_paths = self.imported_paths.clone();
        self.name.push(format!(
            "block@{}:{}",
            block.span().start().line,
            block.span().start().column
        ));
        let items = block.stmts.iter().filter_map(|stmt| match stmt {
            syn::Stmt::Item(item) => Some(item),
            _ => None,
        });
        self.imported_paths
            .extend(imported_paths(items.clone(), &self.scope));
        self.imported_sleeps.extend(imported_sleeps(items.clone()));
        for item in items {
            if let syn::Item::Fn(function) = item {
                let name = function.sig.ident.to_string();
                self.imported_paths.insert(
                    name.clone(),
                    std::iter::once("crate".to_owned())
                        .chain(self.name.iter().cloned())
                        .chain([name])
                        .collect(),
                );
            }
        }
        syn::visit::visit_block(self, block);
        self.name.pop();
        self.imported_sleeps = outer;
        self.imported_paths = outer_paths;
    }

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
        syn::visit::visit_expr_method_call(self, call);
        self.callees.push(Call {
            path: vec![call.method.to_string()],
            method: true,
            paused: self.paused,
            in_test: self.in_test,
        });
    }

    /// `join!` runs every future it's given, so its tokens run in order. Other macros,
    /// such as `select!`, may run only some of theirs.
    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        let name = mac.path.segments.last().map(|seg| seg.ident.to_string());
        match name.as_deref() {
            Some("join" | "try_join") => self.tokens(mac.tokens.clone()),
            _ => self.may_not_run(|body| body.tokens(mac.tokens.clone())),
        }
        if matches!(
            name.as_deref(),
            Some("panic" | "unreachable" | "todo" | "unimplemented")
        ) {
            self.diverged = true;
        }
    }
}

fn imported_paths<'a>(
    items: impl IntoIterator<Item = &'a syn::Item>,
    scope: &[String],
) -> HashMap<String, Vec<String>> {
    fn tree(
        use_tree: &syn::UseTree,
        prefix: &mut Vec<String>,
        out: &mut HashMap<String, Vec<String>>,
    ) {
        match use_tree {
            syn::UseTree::Path(path) => {
                prefix.push(path.ident.to_string());
                tree(&path.tree, prefix, out);
                prefix.pop();
            }
            syn::UseTree::Name(name) => {
                let mut path = prefix.clone();
                if name.ident != "self" {
                    path.push(name.ident.to_string());
                }
                if let Some(last) = path.last() {
                    out.insert(last.clone(), path);
                }
            }
            syn::UseTree::Rename(rename) => {
                let mut path = prefix.clone();
                if rename.ident != "self" {
                    path.push(rename.ident.to_string());
                }
                out.insert(rename.rename.to_string(), path);
            }
            syn::UseTree::Group(group) => {
                for item in &group.items {
                    tree(item, prefix, out);
                }
            }
            syn::UseTree::Glob(_) => {}
        }
    }
    let mut out = HashMap::new();
    for item in items {
        if let syn::Item::Use(item_use) = item {
            tree(&item_use.tree, &mut Vec::new(), &mut out);
        }
    }
    for path in out.values_mut() {
        let mut base = scope.to_vec();
        match path.first().map(String::as_str) {
            Some("crate" | "std" | "tokio") => continue,
            Some("self") => {
                path.remove(0);
            }
            Some("super") => {
                while path.first().is_some_and(|part| part == "super") {
                    base.pop();
                    path.remove(0);
                }
            }
            _ => {}
        }
        *path = std::iter::once("crate".to_owned())
            .chain(base)
            .chain(path.drain(..))
            .collect();
    }
    out
}

fn imported_sleeps<'a>(items: impl IntoIterator<Item = &'a syn::Item>) -> HashMap<String, Clock> {
    fn tree(
        use_tree: &syn::UseTree,
        prefix: &mut Vec<String>,
        globs: &mut HashMap<String, Clock>,
        out: &mut HashMap<String, Clock>,
    ) {
        let clock = if prefix.iter().any(|part| part == "thread") {
            Clock::Thread
        } else {
            Clock::Tokio
        };
        let is_sleep = |ident: &syn::Ident| ident == "sleep" || ident == "sleep_until";
        match use_tree {
            syn::UseTree::Path(path) => {
                prefix.push(path.ident.to_string());
                tree(&path.tree, prefix, globs, out);
                prefix.pop();
            }
            syn::UseTree::Name(name) if is_sleep(&name.ident) => {
                out.insert(name.ident.to_string(), clock);
            }
            syn::UseTree::Rename(rename) if is_sleep(&rename.ident) => {
                out.insert(rename.rename.to_string(), clock);
            }
            syn::UseTree::Group(group) => {
                for item in &group.items {
                    tree(item, prefix, globs, out);
                }
            }
            syn::UseTree::Glob(_) => {
                let path: Vec<_> = prefix.iter().map(String::as_str).collect();
                match path.as_slice() {
                    ["std", "thread"] => {
                        globs.insert("sleep".to_owned(), Clock::Thread);
                    }
                    ["tokio", "time"] => {
                        globs.insert("sleep".to_owned(), Clock::Tokio);
                        globs.insert("sleep_until".to_owned(), Clock::Tokio);
                    }
                    _ => {}
                }
            }
            syn::UseTree::Name(_) | syn::UseTree::Rename(_) => {}
        }
    }
    let mut out = HashMap::new();
    let mut globs = HashMap::new();
    for item in items {
        if let syn::Item::Use(item_use) = item {
            tree(&item_use.tree, &mut Vec::new(), &mut globs, &mut out);
        }
    }
    globs.extend(out);
    globs
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
        if retry() { settle().await; }
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
    fn an_import_reaches_only_its_own_module() {
        let source = r#"
mod paused {
    use tokio::time::sleep;

    #[tokio::test(start_paused = true)]
    async fn waits() {
        sleep(Duration::from_secs(1)).await;
    }
}

mod real {
    use std::thread::sleep;

    #[test]
    fn waits() {
        sleep(Duration::from_millis(5));
    }
}
"#;
        assert_eq!(lint(source), ["16: real-clock sleep in test code"]);
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
    fn a_later_pause_leaves_an_earlier_sleep_real() {
        let source = r#"
#[tokio::test]
async fn waits() {
    tokio::time::sleep(Duration::from_secs(1)).await;
    tokio::time::pause();
}
"#;
        assert_eq!(lint(source), ["4: real-clock sleep in test code"]);
    }

    #[test]
    fn a_pause_that_may_not_run_leaves_a_later_sleep_real() {
        let source = r#"
#[tokio::test]
async fn waits() {
    if ready() { tokio::time::pause(); }
    tokio::time::sleep(d).await;
    match mode { Mode::Fake => tokio::time::pause(), Mode::Real => {} }
    tokio::time::sleep(d).await;
    for _ in 0..n { tokio::time::pause(); }
    tokio::time::sleep(d).await;
    let pause = || tokio::time::pause();
    tokio::time::sleep(d).await;
    tokio::select! { _ = ready() => tokio::time::pause(), _ = other() => {} }
    tokio::time::sleep(d).await;
}
"#;
        assert_eq!(
            lint(source),
            [
                "5: real-clock sleep in test code",
                "7: real-clock sleep in test code",
                "9: real-clock sleep in test code",
                "11: real-clock sleep in test code",
                "13: real-clock sleep in test code",
            ]
        );
    }

    #[test]
    fn a_pause_covers_the_rest_of_its_own_branch() {
        let source = r#"
#[tokio::test]
async fn waits() {
    if ready() {
        tokio::time::pause();
        tokio::time::sleep(d).await;
    }
    loop {
        tokio::time::sleep(d).await;
        tokio::time::pause();
        tokio::time::sleep(d).await;
    }
    tokio::join!(async { tokio::time::pause(); tokio::time::sleep(d).await; });
    { tokio::time::pause(); }
    tokio::time::sleep(d).await;
}
"#;
        assert_eq!(lint(source), ["9: real-clock sleep in test code"]);
    }

    #[test]
    fn a_failed_match_guard_runs_before_the_next_arm() {
        let source = r#"
#[tokio::test(start_paused = true)]
async fn waits() {
    match 0 {
        _ if { tokio::time::resume(); false } => (),
        _ => tokio::time::sleep(d).await,
    }
}
"#;
        assert_eq!(lint(source), ["6: real-clock sleep in test code"]);
    }

    #[test]
    fn a_break_out_of_a_labeled_block_falls_through_after_it() {
        let source = r#"
#[tokio::test]
async fn waits() {
    if ready() {
        'done: {
            tokio::time::pause();
            break 'done;
        }
    } else {
        return;
    }
    tokio::time::sleep(d).await;
}
"#;
        assert_eq!(lint(source), Vec::<String>::new());
    }

    #[test]
    fn an_awaited_async_block_ends_at_its_returns() {
        let paused = r#"
#[tokio::test]
async fn waits() {
    async {
        if ready() { tokio::time::pause(); return; } else { tokio::time::pause(); return; }
    }.await;
    tokio::time::sleep(d).await;
}
"#;
        assert_eq!(lint(paused), Vec::<String>::new());
        let early = r#"
#[tokio::test]
async fn waits() {
    async {
        if ready() { return; }
        tokio::time::pause();
    }.await;
    tokio::time::sleep(d).await;
}
"#;
        assert_eq!(lint(early), ["8: real-clock sleep in test code"]);
    }

    #[test]
    fn a_resume_that_may_run_ends_paused_time() {
        let source = r#"
#[tokio::test(start_paused = true)]
async fn waits() {
    if ready() { tokio::time::resume(); }
    tokio::time::sleep(d).await;
}
"#;
        assert_eq!(lint(source), ["5: real-clock sleep in test code"]);
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
    fn same_named_helpers_keep_their_module_clock() {
        let source = r#"
#[cfg(test)]
mod a {
    async fn waits() { tokio::time::sleep(d).await; }
    #[tokio::test(start_paused = true)]
    async fn test() { waits().await; }
}
#[cfg(test)]
mod b {
    async fn waits() { tokio::time::sleep(d).await; }
    #[tokio::test]
    async fn test() { waits().await; }
}
"#;
        assert_eq!(lint(source), ["10: real-clock sleep in test code"]);
    }

    #[test]
    fn scans_cfg_test_code_inside_production_functions() {
        let source = r#"
fn production() {
    std::thread::sleep(d);
    #[cfg(test)]
    { std::thread::sleep(d); }
    #[cfg(test)]
    if ready() { std::thread::sleep(d); }
    #[cfg(test)]
    let _wait = std::thread::sleep(d);
    std::thread::sleep(d);
}
"#;
        assert_eq!(
            lint(source),
            [
                "5: real-clock sleep in test code",
                "7: real-clock sleep in test code",
                "9: real-clock sleep in test code",
            ]
        );
    }

    #[test]
    fn follows_function_local_sleep_imports() {
        let source = r#"
#[tokio::test(start_paused = true)]
async fn waits() {
    use std::thread::sleep as wait;
    wait(d);
    {
        use tokio::time::sleep as wait;
        wait(d).await;
    }
    wait(d);
}
"#;
        assert_eq!(
            lint(source),
            [
                "5: real-clock sleep in test code",
                "10: real-clock sleep in test code",
            ]
        );
    }

    #[test]
    fn helper_calls_keep_the_clock_at_the_call() {
        let source = r#"
#[cfg(test)]
mod tests {
    async fn before() { tokio::time::sleep(d).await; }
    async fn after() { tokio::time::sleep(d).await; }
    #[tokio::test]
    async fn waits() {
        before().await;
        tokio::time::pause();
        after().await;
    }
}
"#;
        assert_eq!(lint(source), ["4: real-clock sleep in test code"]);
    }

    #[test]
    fn a_helper_called_after_a_conditional_pause_is_real() {
        let source = r#"
#[cfg(test)]
mod tests {
    async fn helper() { tokio::time::sleep(d).await; }
    #[tokio::test]
    async fn waits() {
        if ready() { tokio::time::pause(); }
        helper().await;
        tokio::time::sleep(d).await;
    }
}
"#;
        assert_eq!(
            lint(source),
            [
                "4: real-clock sleep in test code",
                "9: real-clock sleep in test code"
            ]
        );
    }

    #[test]
    fn thread_glob_sleeps_stay_real_on_paused_time() {
        let source = r#"
use std::thread::*;
#[tokio::test(start_paused = true)]
async fn waits() { sleep(d); }
"#;
        assert_eq!(lint(source), ["4: real-clock sleep in test code"]);
    }

    #[test]
    fn a_pause_on_only_some_paths_leaves_a_later_sleep_real() {
        for setup in [
            "if ready() { tokio::time::pause(); }",
            "match flag { true => tokio::time::pause(), false => {} }",
            "while ready() { tokio::time::pause(); }",
            "for _ in values { tokio::time::pause(); }",
            "let later = || { tokio::time::pause(); };",
            "let later = async { tokio::time::pause(); };",
            "let _ = ready() && { tokio::time::pause(); true };",
            "tokio::select! { _ = ready() => { tokio::time::pause(); } }",
        ] {
            let source = format!(
                "#[tokio::test]\nasync fn waits() {{ {setup}\ntokio::time::sleep(d).await; }}"
            );
            assert_eq!(
                lint(&source),
                ["3: real-clock sleep in test code"],
                "{setup}"
            );
        }
    }

    #[test]
    fn branch_clocks_join_where_the_branches_do() {
        let source = r#"
#[tokio::test]
async fn waits() {
    if ready() {
        tokio::time::pause();
        tokio::time::sleep(d).await;
    } else {
        tokio::time::sleep(d).await;
    }
    if ready() { tokio::time::pause(); } else { tokio::time::pause(); }
    tokio::time::sleep(d).await;
    match flag { true => {}, false => tokio::time::resume() }
    tokio::time::sleep(d).await;
}
"#;
        assert_eq!(
            lint(source),
            [
                "8: real-clock sleep in test code",
                "13: real-clock sleep in test code"
            ]
        );
    }

    #[test]
    fn resolves_qualified_helpers_and_associated_functions() {
        let source = r#"
#[cfg(test)]
mod tests {
    async fn helper() { tokio::time::sleep(d).await; }
    struct Cloud;
    impl Cloud {
        async fn delay() { helper().await; }
    }
    mod nested {
        #[tokio::test(start_paused = true)]
        async fn waits() {
            super::Cloud::delay().await;
            crate::tests::helper().await;
        }
    }
    #[tokio::test(start_paused = true)]
    async fn waits() { self::helper().await; }
}
"#;
        assert!(lint(source).is_empty(), "{:?}", lint(source));
    }

    #[test]
    fn same_named_impl_helpers_keep_their_clock() {
        let source = r#"
#[cfg(test)]
mod tests {
    struct A;
    impl A { async fn waits() { tokio::time::sleep(d).await; } }
    struct B;
    impl B { async fn waits() { tokio::time::sleep(d).await; } }
    #[tokio::test(start_paused = true)]
    async fn paused() { A::waits().await; }
    #[tokio::test]
    async fn real() { B::waits().await; }
}
"#;
        assert_eq!(lint(source), ["7: real-clock sleep in test code"]);
    }

    #[test]
    fn explicit_sleep_imports_override_globs() {
        let source = r#"
use tokio::time::sleep;
use std::thread::*;
use unrelated::*;
#[tokio::test(start_paused = true)]
async fn waits() {
    sleep(d).await;
    {
        use tokio::time::*;
        sleep(d).await;
    }
}
"#;
        assert!(lint(source).is_empty(), "{:?}", lint(source));
    }

    #[test]
    fn unrelated_local_globs_do_not_shadow_thread_sleep() {
        let source = r#"
use std::thread::sleep;
#[tokio::test(start_paused = true)]
async fn waits() {
    use std::time::*;
    sleep(d);
}
"#;
        assert_eq!(lint(source), ["6: real-clock sleep in test code"]);
    }

    #[test]
    fn resolves_methods_and_imported_helpers() {
        let source = r#"
#[cfg(test)]
mod tests {
    struct Worker;
    impl Worker { async fn wait(&self) { tokio::time::sleep(d).await; } }
    mod support { pub async fn helper() { tokio::time::sleep(d).await; } }
    use support::helper;
    #[tokio::test(start_paused = true)]
    async fn waits() {
        use support::helper as local;
        Worker.wait().await;
        helper().await;
        local().await;
    }
}
"#;
        assert!(lint(source).is_empty(), "{:?}", lint(source));
    }

    #[test]
    fn imported_and_unresolved_real_callers_keep_helpers_real() {
        for real_call in ["settle().await", "unknown::settle().await"] {
            let source = format!(
                r#"
#[cfg(test)]
mod common {{ pub async fn settle() {{ tokio::time::sleep(d).await; }} }}
#[tokio::test(start_paused = true)]
async fn paused() {{ crate::common::settle().await; }}
#[tokio::test]
async fn real() {{ use crate::common::settle; {real_call}; }}
"#
            );
            assert_eq!(
                lint(&source),
                ["3: real-clock sleep in test code"],
                "{real_call}"
            );
        }
    }

    #[test]
    fn method_callers_are_not_hidden_by_same_named_free_functions() {
        let source = r#"
#[cfg(test)]
mod tests {
    struct Worker;
    impl Worker { async fn wait(&self) { tokio::time::sleep(d).await; } }
    async fn wait() {}
    #[tokio::test(start_paused = true)]
    async fn paused() { Worker::wait(&Worker).await; }
    #[tokio::test]
    async fn real() { Worker.wait().await; }
}
"#;
        assert_eq!(lint(source), ["5: real-clock sleep in test code"]);
    }

    #[test]
    fn production_calls_do_not_decide_a_test_helpers_clock() {
        let source = r#"
async fn production(child: &mut tokio::process::Child) {
    child.wait().await.unwrap();
}
#[cfg(test)]
mod tests {
    struct Worker;
    impl Worker { async fn wait(&self) { tokio::time::sleep(d).await; } }
    #[tokio::test(start_paused = true)]
    async fn paused() { Worker.wait().await; }
}
"#;
        assert_eq!(lint(source), Vec::<String>::new());
    }

    #[test]
    fn cfg_test_context_stays_in_its_arm_or_field() {
        let source = r#"
fn production() {
    match x {
        #[cfg(test)] 0 => std::thread::sleep(d),
        _ => std::thread::sleep(d),
    }
    S {
        #[cfg(test)] a: std::thread::sleep(d),
        b: std::thread::sleep(d),
    };
}
"#;
        assert_eq!(
            lint(source),
            [
                "4: real-clock sleep in test code",
                "8: real-clock sleep in test code"
            ]
        );
    }

    #[test]
    fn loop_and_async_pauses_follow_visit_order() {
        for setup in [
            "loop { if ready() { tokio::time::pause(); break; } yield_now().await; }",
            "loop { tokio::time::pause(); if ready() { break; } else { break; } }",
            "async { tokio::time::pause(); }.await;",
            "(async { tokio::time::pause(); }).await;",
            "'outer: loop { loop { tokio::time::pause(); break 'outer; } }",
        ] {
            let source = format!(
                "#[tokio::test]\nasync fn waits() {{ {setup}\ntokio::time::sleep(d).await; }}"
            );
            assert!(lint(&source).is_empty(), "{setup}: {:?}", lint(&source));
        }
    }

    #[test]
    fn loop_resumes_follow_visit_order() {
        for body in [
            "if ready() { break; } tokio::time::resume();",
            "if ready() { tokio::time::resume(); continue; } break;",
        ] {
            let source = format!(
                "#[tokio::test(start_paused = true)]\nasync fn waits() {{ loop {{ {body} }}\ntokio::time::sleep(d).await; }}"
            );
            assert_eq!(
                lint(&source),
                ["3: real-clock sleep in test code"],
                "{body}"
            );
        }
    }

    #[test]
    fn let_else_pause_does_not_reach_following_code() {
        let source = r#"
#[tokio::test]
async fn waits() {
    let Some(value) = value else { tokio::time::pause(); return; };
    tokio::time::sleep(d).await;
}
"#;
        assert_eq!(lint(source), ["5: real-clock sleep in test code"]);
    }

    #[test]
    fn nested_helpers_use_call_site_clocks() {
        let source = r#"
#[tokio::test]
async fn waits() {
    helper().await;
    tokio::time::pause();
    async fn helper() { tokio::time::sleep(d).await; }
}
#[tokio::test(start_paused = true)]
async fn paused() {
    helper().await;
    async fn helper() { tokio::time::sleep(d).await; }
}
"#;
        assert_eq!(lint(source), ["6: real-clock sleep in test code"]);
    }

    #[test]
    fn nested_helpers_keep_block_imports_and_clocks() {
        let source = r#"
#[cfg(test)]
mod tests {
    mod real { pub async fn helper() { tokio::time::sleep(d).await; } }
    mod virtual_time { pub async fn helper() { tokio::time::sleep(d).await; } }
    #[tokio::test]
    async fn waits() {
        {
            use real::helper;
            nested().await;
            async fn nested() { helper().await; }
        }
        tokio::time::pause();
        {
            use virtual_time::helper;
            nested().await;
            async fn nested() { helper().await; }
        }
    }
}
"#;
        assert_eq!(lint(source), ["4: real-clock sleep in test code"]);
    }

    #[test]
    fn inner_globs_override_outer_sleep_imports() {
        let source = r#"
use std::thread::sleep;
#[tokio::test(start_paused = true)]
async fn waits() {
    { use tokio::time::*; sleep(d).await; }
    { use tokio::time::sleep; { use std::thread::*; sleep(d); } }
}
"#;
        assert_eq!(lint(source), ["6: real-clock sleep in test code"]);
    }

    #[test]
    fn renamed_thread_module_sleep_stays_real() {
        let source = r#"
use std::thread as th;
#[tokio::test(start_paused = true)]
async fn waits() { th::sleep(d); }
"#;
        assert_eq!(lint(source), ["4: real-clock sleep in test code"]);
    }

    #[test]
    fn round2_future_awaited_after_resume_stays_real() {
        let source = r#"
#[cfg(test)]
mod tests {
    async fn helper() { tokio::time::sleep(d).await; }
    #[tokio::test(start_paused = true)]
    async fn waits() {
        let future = helper();
        tokio::time::resume();
        future.await;
    }
}
"#;
        assert_eq!(lint(source), ["4: real-clock sleep in test code"]);
    }

    #[test]
    fn round2_loops_without_visible_breaks_keep_paused_time() {
        for setup in [
            "tokio::spawn(async { loop { tokio::time::sleep(d).await; } });",
            "loop { tokio::select! { _ = event() => break } }",
            "let result: Result<(), E> = async { loop { event()?; } }.await;",
            "'ready: { loop { break 'ready; } }",
        ] {
            let source = format!(
                "#[tokio::test(start_paused = true)]\nasync fn waits() {{ {setup}\ntokio::time::sleep(d).await; }}"
            );
            assert_eq!(lint(&source), Vec::<String>::new(), "{setup}");
        }
    }

    #[test]
    fn round2_nonlocal_exits_do_not_revisit_loop_sleeps() {
        for body in [
            "tokio::time::sleep(d).await; loop { break 'outer; }",
            "tokio::time::sleep(d).await; if ready() { tokio::time::resume(); return; } break;",
        ] {
            let source = format!(
                "#[tokio::test(start_paused = true)]\nasync fn waits() {{ 'outer: loop {{ {body} }} }}"
            );
            assert_eq!(lint(&source), Vec::<String>::new(), "{body}");
        }
    }

    #[test]
    fn round2_returning_branch_keeps_visit_order_clock() {
        for setup in [
            "if skip { return; } else { tokio::time::pause(); }",
            "match skip { true => return, false => tokio::time::pause() }",
        ] {
            let source = format!(
                "#[tokio::test]\nasync fn waits() {{ {setup}\ntokio::time::sleep(d).await; }}"
            );
            assert_eq!(lint(&source), Vec::<String>::new(), "{setup}");
        }
    }

    #[test]
    fn round2_production_method_only_falls_back_to_receiver_methods() {
        let source = r#"
async fn production(child: &mut tokio::process::Child) {
    child.wait().await;
}
#[cfg(test)]
mod tests {
    async fn wait() { tokio::time::sleep(d).await; }
    struct Worker;
    impl Worker { async fn wait() { tokio::time::sleep(d).await; } }
    #[tokio::test(start_paused = true)]
    async fn waits() { wait().await; Worker::wait().await; }
}
"#;
        assert_eq!(lint(source), Vec::<String>::new());
    }

    #[test]
    fn round2_join_pause_reaches_following_sleep() {
        let source = r#"
#[tokio::test]
async fn waits() {
    tokio::join!(async { tokio::time::pause(); });
    tokio::time::sleep(d).await;
}
"#;
        assert_eq!(lint(source), Vec::<String>::new());
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
