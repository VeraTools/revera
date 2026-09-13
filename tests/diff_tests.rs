use revera::diff::{parse_unified, DiffLineKind, FileStatus};

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
