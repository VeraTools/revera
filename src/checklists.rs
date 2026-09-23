//! Built-in defect checklists per file type, added to reviewer prompts for
//! the kinds of files a change touches. They list failure modes only: style
//! and naming stay out, as the reviewer prompts require.

/// A checklist: its name, its text, and whether it applies to a path.
type Checklist = (&'static str, &'static str, fn(&str) -> bool);

const CHECKLISTS: &[Checklist] = &[
    ("rust", include_str!("../prompts/checklists/rust.md"), |p| {
        p.ends_with(".rs")
    }),
    (
        "python",
        include_str!("../prompts/checklists/python.md"),
        |p| p.ends_with(".py"),
    ),
    (
        "typescript/javascript",
        include_str!("../prompts/checklists/typescript.md"),
        |p| {
            [".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs"]
                .iter()
                .any(|s| p.ends_with(s))
        },
    ),
    ("go", include_str!("../prompts/checklists/go.md"), |p| {
        p.ends_with(".go")
    }),
    (
        "java/kotlin",
        include_str!("../prompts/checklists/java.md"),
        |p| [".java", ".kt", ".kts"].iter().any(|s| p.ends_with(s)),
    ),
    ("sql", include_str!("../prompts/checklists/sql.md"), |p| {
        p.ends_with(".sql")
    }),
    (
        "shell",
        include_str!("../prompts/checklists/shell.md"),
        |p| [".sh", ".bash", ".zsh"].iter().any(|s| p.ends_with(s)),
    ),
    ("c/c++", include_str!("../prompts/checklists/c.md"), |p| {
        [".c", ".h", ".cc", ".cpp", ".cxx", ".hpp", ".hh"]
            .iter()
            .any(|s| p.ends_with(s))
    }),
    (
        "github actions",
        include_str!("../prompts/checklists/github-actions.md"),
        |p| p.starts_with(".github/workflows/") && (p.ends_with(".yml") || p.ends_with(".yaml")),
    ),
];

/// Every checklist text, for the prompt version hash.
pub fn all_texts() -> impl Iterator<Item = &'static str> {
    CHECKLISTS.iter().map(|(_, text, _)| *text)
}

/// Checklists that apply to `changed`, in fixed order (stable prompt bytes).
pub fn select(changed: &[String]) -> Vec<(&'static str, &'static str)> {
    CHECKLISTS
        .iter()
        .filter(|(_, _, m)| changed.iter().any(|p| m(&p.to_ascii_lowercase())))
        .map(|(name, text, _)| (*name, *text))
        .collect()
}

/// Prompt section for `selected`; empty when nothing applies.
pub fn render(selected: &[(&str, &str)]) -> String {
    if selected.is_empty() {
        return String::new();
    }
    let mut s = String::from(
        "\n\nDefect checklists for the changed file types (common failure modes; report only concrete instances this diff introduces, with a trigger and evidence, never the checklist itself):\n",
    );
    for (name, text) in selected {
        s.push_str(&format!("\n{name}:\n{}\n", text.trim()));
    }
    s
}
