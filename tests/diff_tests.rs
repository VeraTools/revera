use revera::diff::{DiffLineKind, FileStatus, parse_unified};

const BASIC: &str = "\
diff --git a/src/a.rs b/src/a.rs
index 111..222 100644
--- a/src/a.rs
+++ b/src/a.rs
@@ -1,3 +1,4 @@ fn f()
 fn a() {}
-old
+new
+newer
 ctx
";

#[test]
fn parses_basic_hunk() {
    let d = parse_unified(BASIC);
    assert_eq!(d.files.len(), 1);
    let f = &d.files[0];
    assert_eq!(f.new_path, "src/a.rs");
    assert_eq!(f.status, FileStatus::Modified);
    let h = &f.hunks[0];
    assert_eq!(
        (h.old_start, h.old_len, h.new_start, h.new_len),
        (1, 3, 1, 4)
    );
    // kinds and line numbers
    assert_eq!(h.lines[0].kind, DiffLineKind::Ctx);
    assert_eq!(h.lines[1].kind, DiffLineKind::Del);
    assert_eq!(h.lines[1].old_no, Some(2));
    assert_eq!(h.lines[2].new_no, Some(2));
    assert_eq!(h.lines[3].new_no, Some(3));
    assert_eq!(h.lines[4].new_no, Some(4));
}

#[test]
fn head_side_lines() {
    let d = parse_unified(BASIC);
    let s = d.head_side_lines("src/a.rs");
    assert!(s.contains(&1) && s.contains(&2) && s.contains(&3) && s.contains(&4));
    assert_eq!(s.len(), 4);
}

#[test]
fn rename_and_new_and_deleted() {
    let t = "\
diff --git a/old.rs b/new.rs
similarity index 90%
rename from old.rs
rename to new.rs
--- a/old.rs
+++ b/new.rs
@@ -1,1 +1,1 @@
-x
+y
diff --git a/added.rs b/added.rs
new file mode 100644
--- /dev/null
+++ b/added.rs
@@ -0,0 +1,1 @@
+hi
diff --git a/gone.rs b/gone.rs
deleted file mode 100644
--- a/gone.rs
+++ /dev/null
@@ -1,1 +0,0 @@
-bye
";
    let d = parse_unified(t);
    assert_eq!(d.files.len(), 3);
    assert_eq!(d.files[0].status, FileStatus::Renamed);
    assert_eq!(d.files[0].old_path, "old.rs");
    assert_eq!(d.files[0].new_path, "new.rs");
    assert_eq!(d.files[1].status, FileStatus::Added);
    assert_eq!(d.files[1].new_path, "added.rs");
    assert_eq!(d.files[2].status, FileStatus::Deleted);
    // deleted file has no head-side lines
    assert!(d.head_side_lines("gone.rs").is_empty());
    // added file's new lines are head-side
    assert!(d.head_side_lines("added.rs").contains(&1));
}

#[test]
fn no_newline_marker() {
    let t = "\
diff --git a/f b/f
--- a/f
+++ b/f
@@ -1,1 +1,1 @@
-old
\\ No newline at end of file
+new
\\ No newline at end of file
";
    let d = parse_unified(t);
    assert_eq!(d.files[0].hunks[0].lines.len(), 2);
    assert_eq!(d.files[0].hunks[0].lines[1].kind, DiffLineKind::Add);
}

#[test]
fn hunk_containing() {
    let d = parse_unified(BASIC);
    let h = d.hunk_containing("src/a.rs", 3).unwrap();
    assert!(h.contains("newer") && h.contains('+'));
    assert!(d.hunk_containing("src/a.rs", 99).is_none());
}

#[test]
fn diff_context_exact_hit_is_unchanged() {
    let d = parse_unified(BASIC);
    assert_eq!(
        d.diff_context("src/a.rs", 3).unwrap(),
        concat!(
            "@@ -1,3 +1,4 @@ src/a.rs\n",
            "           1:1 fn a() {}\n",
            "-      2->     old\n",
            "+          ->2 new\n",
            "+          ->3 newer\n",
            "           3:4 ctx\n",
        )
    );
}

#[test]
fn diff_context_nearest_hunk_within_40_lines() {
    let d = parse_unified(&format!("{BASIC}@@ -88,2 +88,2 @@\n second\n hunk\n"));
    for (line, start, end) in [(0, 1, 4), (44, 1, 4), (48, 88, 89), (129, 88, 89)] {
        assert_eq!(
            d.diff_context("src/a.rs", line).unwrap(),
            format!(
                "Requested line src/a.rs:{line} is outside the diff; showing head-side range +{start}..{end}.\n{}",
                d.hunk_containing("src/a.rs", start).unwrap()
            )
        );
    }
    assert!(d.diff_context("src/a.rs", 130).is_err());
}

#[test]
fn diff_context_distant_miss_lists_at_most_20_ranges() {
    let mut text = BASIC.to_string();
    for start in 100..=120 {
        text.push_str(&format!("@@ -{start},1 +{start},1 @@\n context\n"));
    }
    let d = parse_unified(&text);
    let error = d.diff_context("src/a.rs", 999).unwrap_err();
    assert!(error.starts_with(
        "no hunk within 40 lines of src/a.rs:999; head-side hunk ranges: +1..4, +100..100"
    ));
    assert!(error.ends_with("+118..118, …"));
    assert!(!error.contains("+119..119"));
}

#[test]
fn diff_context_unchanged_file_suggests_read_file_and_renames_match() {
    let d = parse_unified(&BASIC.replace("a/src/a.rs", "a/src/old.rs"));
    assert_eq!(
        d.diff_context("src/unchanged.rs", 1).unwrap_err(),
        "src/unchanged.rs is not changed in this pull request; use read_file"
    );
    assert_eq!(
        d.diff_context("src/old.rs", 3),
        d.diff_context("src/a.rs", 3)
    );
    assert!(d.diff_context("src/old.rs", 5).unwrap().contains("+1..4"));
}

#[test]
fn diff_context_handles_empty_head_ranges_and_hunkless_files() {
    let d = parse_unified(
        "diff --git a/gone b/gone\n@@ -1,1 +0,0 @@\n-deleted\ndiff --git a/empty b/empty\n",
    );
    assert!(d.diff_context("gone", 1).unwrap().contains("+0,0 (empty)"));
    assert!(
        d.diff_context("gone", 41)
            .unwrap_err()
            .ends_with("+0,0 (empty)")
    );
    assert!(
        d.diff_context("empty", 1)
            .unwrap_err()
            .ends_with("ranges: none")
    );
}

fn added_file(path: &str, text: &str) -> String {
    format!("diff --git a/{path} b/{path}\n@@ -0,0 +1,1 @@\n+{text}\n")
}

#[test]
fn truncation_keeps_source_before_docs_and_omitted_hunks_remain_available() {
    let d = parse_unified(&format!(
        "{}{}",
        added_file("docs/a.md", &"prose ".repeat(100)),
        added_file("src/z.rs", "fn changed() {}")
    ));
    let budget = d.file_excerpt("docs/a.md").len();
    let rendered = d.render_truncated(budget);
    assert_eq!(d.omitted_files(budget), ["docs/a.md"]);
    assert!(rendered.contains("+fn changed() {}"));
    assert!(!rendered.contains("+prose"));
    assert!(rendered.contains("hunks still available through diff_context"));
    assert!(d.hunk_containing("docs/a.md", 1).unwrap().contains("prose"));
    assert!(d.diff_context("docs/a.md", 1).unwrap().contains("prose"));
}

#[test]
fn truncation_preserves_output_and_within_class_order_and_agrees_on_omissions() {
    let paths = [
        "docs/a.md",
        "src/z.rs",
        "src/a.rs",
        "tests/z.rs",
        "Cargo.toml",
        "Cargo.lock",
    ];
    let text: String = paths.iter().map(|path| added_file(path, path)).collect();
    let d = parse_unified(&text);
    let budget = d.file_excerpt("src/z.rs").len();
    let rendered = d.render_truncated(budget);
    assert!(rendered.contains("+src/z.rs\n"));
    assert!(!rendered.contains("+src/a.rs\n"));
    let sections: Vec<_> = rendered.split("diff --git ").skip(1).collect();
    let rendered_paths: Vec<_> = sections
        .iter()
        .map(|s| s.lines().next().unwrap().split(" b/").nth(1).unwrap())
        .collect();
    assert_eq!(rendered_paths, paths);
    let omitted: Vec<_> = sections
        .iter()
        .zip(&rendered_paths)
        .filter(|(s, _)| s.contains("[file diff omitted:"))
        .map(|(_, path)| *path)
        .collect();
    assert_eq!(d.omitted_files(budget), omitted);
    let budget: usize = paths[1..=4]
        .iter()
        .map(|path| d.file_excerpt(path).len())
        .sum();
    let rendered = d.render_truncated(budget);
    for path in &paths[1..=4] {
        assert!(rendered.contains(&format!("+{path}\n")), "{path}");
    }
    assert_eq!(d.omitted_files(budget), ["docs/a.md", "Cargo.lock"]);
    assert_eq!(d.render_truncated(usize::MAX), d.render());
}
