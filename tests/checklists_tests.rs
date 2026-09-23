use revera::checklists::{all_texts, render, select};

fn names(paths: &[&str]) -> Vec<&'static str> {
    let changed: Vec<String> = paths.iter().map(|p| p.to_string()).collect();
    select(&changed).into_iter().map(|(n, _)| n).collect()
}

#[test]
fn checklists_follow_the_changed_file_types() {
    assert_eq!(
        names(&["src/a.rs", "web/App.TSX"]),
        vec!["rust", "typescript/javascript"]
    );
    assert_eq!(names(&[".github/workflows/ci.yml"]), vec!["github actions"]);
    // a YAML file outside workflows is not an Actions workflow
    assert!(names(&["config/ci.yml"]).is_empty());
    assert!(names(&["README.md", "docs/x.md"]).is_empty());
    assert_eq!(render(&[]), "");
}

#[test]
fn checklists_list_defects_not_style() {
    for text in all_texts() {
        let lower = text.to_ascii_lowercase();
        for banned in ["naming", "style", "formatting", "typo", "spelling"] {
            assert!(!lower.contains(banned), "{banned:?} in checklist:\n{text}");
        }
    }
    let rendered = render(&select(&["src/a.rs".to_string()]));
    assert!(
        rendered.contains("never the checklist itself"),
        "{rendered}"
    );
}
