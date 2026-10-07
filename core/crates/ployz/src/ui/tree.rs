//! What a destructive command takes with it, as a short tree shown before it asks.

use std::fmt;

use super::Tone;

/// A line, and the lines under it.
pub(crate) struct Tree {
    label: String,
    children: Vec<Self>,
}

impl Tree {
    pub(crate) fn new(label: impl Into<String>, children: Vec<Self>) -> Self {
        Self {
            label: label.into(),
            children,
        }
    }

    pub(crate) fn leaf(label: impl Into<String>) -> Self {
        Self::new(label, Vec::new())
    }

    fn write_children(&self, f: &mut fmt::Formatter<'_>, indent: &str) -> fmt::Result {
        for (index, child) in self.children.iter().enumerate() {
            let last = index + 1 == self.children.len();
            let (branch, below) = if last {
                ("└─ ", "   ")
            } else {
                ("├─ ", "│  ")
            };
            writeln!(
                f,
                "{}{}{}",
                Tone::Muted.paint(indent),
                Tone::Muted.paint(branch),
                child.label
            )?;
            child.write_children(f, &format!("{indent}{below}"))?;
        }
        Ok(())
    }
}

impl fmt::Display for Tree {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "{}", self.label)?;
        self.write_children(f, "")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn children_hang_under_their_parent() {
        let tree = Tree::new(
            "Removing Project blog deletes:",
            vec![
                Tree::new(
                    "production",
                    vec![
                        Tree::leaf("Services: api, web"),
                        Tree::leaf("Volumes: pg-data"),
                    ],
                ),
                Tree::new("staging", vec![Tree::leaf("Services: web")]),
            ],
        );
        let text = anstream::adapter::strip_str(&tree.to_string()).to_string();
        assert_eq!(
            text,
            concat!(
                "Removing Project blog deletes:\n",
                "├─ production\n",
                "│  ├─ Services: api, web\n",
                "│  └─ Volumes: pg-data\n",
                "└─ staging\n",
                "   └─ Services: web\n",
            )
        );
    }
}
