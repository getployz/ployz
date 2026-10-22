//! Embeds the user docs: every page under `docs/user` as `PAGES`, and the page
//! list the docs tool's description carries, grouped by folder.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// Pages about writing the docs, not about Ployz.
const SKIPPED: [&str; 2] = ["README.md", "STYLE.md"];

/// The docs, from this crate. Cargo is told what to watch by this relative
/// path, not the absolute one, so the build script's inputs read the same in
/// every worktree and the compiler cache can share this crate between them.
const DOCS: &str = "../../../docs/user";

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets it"));
    let root = manifest.join(DOCS);
    let root = root.canonicalize().unwrap_or_else(|error| {
        panic!(
            "the docs tool embeds {}, which this build cannot read ({error}); copy docs/user into the build context",
            root.display()
        )
    });
    println!("cargo:rerun-if-changed={DOCS}");
    let mut pages = Vec::new();
    walk(&root, &root, &mut pages);

    let mut table = String::from("pub(crate) static PAGES: &[(&str, &str)] = &[\n");
    let mut folders = std::collections::BTreeMap::<&str, Vec<&str>>::new();
    for (path, file) in &pages {
        println!("cargo:rerun-if-changed={DOCS}/{path}");
        let source = std::fs::read_to_string(file).expect("a docs page is UTF-8");
        assert!(title(&source).is_some(), "{path} has no front matter title");
        // The text itself rather than `include_str!` of its absolute path, which
        // would put the checkout's location in the source and give every
        // worktree a different compiler-cache key for this crate.
        writeln!(table, "    ({path:?}, {source:?}),").expect("writing to a String");
        let page = path.strip_suffix(".md").expect("walk keeps .md files");
        let (folder, name) = page.rsplit_once('/').unwrap_or(("", page));
        folders.entry(folder).or_default().push(name);
    }
    table.push_str("];\n");
    let contents = folders
        .iter()
        .map(|(folder, names)| match *folder {
            "" => names.join(", "),
            folder => match names.as_slice() {
                [name] => format!("{folder}/{name}"),
                names => format!("{folder}/{{{}}}", names.join(",")),
            },
        })
        .collect::<Vec<_>>()
        .join(", ");

    let out = PathBuf::from(std::env::var("OUT_DIR").expect("cargo sets it"));
    std::fs::write(out.join("docs_pages.rs"), table).expect("OUT_DIR is writable");
    std::fs::write(out.join("docs_contents.txt"), contents).expect("OUT_DIR is writable");
}

/// Every page under `dir`, as its path from `root` and its file, sorted.
fn walk(dir: &Path, root: &Path, pages: &mut Vec<(String, PathBuf)>) {
    let mut entries = std::fs::read_dir(dir)
        .expect("the docs directory is readable")
        .map(|entry| entry.expect("a docs entry is readable").path())
        .collect::<Vec<_>>();
    entries.sort();
    for file in entries {
        if file.is_dir() {
            println!("cargo:rerun-if-changed={DOCS}/{}", relative(&file, root));
            walk(&file, root, pages);
            continue;
        }
        if file.extension().is_none_or(|extension| extension != "md") {
            continue;
        }
        let path = relative(&file, root);
        if !SKIPPED.contains(&path.as_str()) {
            pages.push((path, file));
        }
    }
}

/// `file`'s path from `root`, with `/` separators.
fn relative(file: &Path, root: &Path) -> String {
    file.strip_prefix(root)
        .expect("walked from root")
        .to_string_lossy()
        .replace('\\', "/")
}

/// The `title:` in a page's front matter.
fn title(source: &str) -> Option<&str> {
    let rest = source.strip_prefix("---\n")?;
    let (front, _) = rest.split_once("\n---")?;
    front
        .lines()
        .find_map(|line| line.strip_prefix("title:"))
        .map(str::trim)
}
