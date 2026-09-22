use revera::config::RuleConfig;
use revera::diff::{DiffLine, DiffLineKind, DiffSet, FileDiff, FileStatus, Hunk};
use revera::findings::Severity;
use revera::rules::scan_diff;

fn make_diff(file: &str, lines: Vec<(DiffLineKind, &str)>) -> DiffSet {
    let diff_lines = lines
        .into_iter()
        .enumerate()
        .map(|(i, (kind, text))| DiffLine {
            kind,
            old_no: match kind {
                DiffLineKind::Add => None,
                _ => Some((i + 1) as u32),
            },
            new_no: match kind {
                DiffLineKind::Del => None,
                _ => Some((i + 1) as u32),
            },
            text: text.to_string(),
        })
        .collect();

    DiffSet {
        files: vec![FileDiff {
            old_path: file.to_string(),
            new_path: file.to_string(),
            status: FileStatus::Modified,
            hunks: vec![Hunk {
                old_start: 1,
                old_len: 10,
                new_start: 1,
                new_len: 10,
                lines: diff_lines,
            }],
        }],
    }
}

#[test]
fn secrets_detector_catches_aws_and_github_keys() {
    let diff = make_diff(
        "src/config.rs",
        vec![
            (DiffLineKind::Ctx, "pub fn init() {"),
            (
                DiffLineKind::Add,
                "    let aws_key = \"AKIAIOSFODNN7EXAMPLE\";",
            ),
            (DiffLineKind::Ctx, "}"),
        ],
    );
    let findings = scan_diff(&diff, None);
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].severity, Severity::High);
    assert!(findings[0].source.contains("secrets"));
    assert_eq!(findings[0].start_line, 2);
}

#[test]
fn debug_detector_only_flags_additions_not_deletions() {
    // Deleted debug statement should NOT trigger
    let diff_del = make_diff(
        "src/lib.rs",
        vec![
            (DiffLineKind::Del, "    dbg!(variable);"),
            (DiffLineKind::Add, "    let clean = true;"),
        ],
    );
    let findings_del = scan_diff(&diff_del, None);
    assert!(findings_del.is_empty());

    // Added debug statement SHOULD trigger
    let diff_add = make_diff(
        "src/lib.rs",
        vec![
            (DiffLineKind::Ctx, "fn process() {"),
            (DiffLineKind::Add, "    dbg!(x);"),
            (DiffLineKind::Ctx, "}"),
        ],
    );
    let findings_add = scan_diff(&diff_add, None);
    assert_eq!(findings_add.len(), 1);
    assert_eq!(findings_add[0].severity, Severity::Low);
    assert!(findings_add[0].source.contains("debug-statement"));
}

#[test]
fn unsafe_detector_flags_unsafe_blocks_in_rust() {
    let diff = make_diff(
        "src/pointer.rs",
        vec![
            (DiffLineKind::Ctx, "fn deref() {"),
            (DiffLineKind::Add, "    unsafe { *ptr = 42; }"),
            (DiffLineKind::Ctx, "}"),
        ],
    );
    let findings = scan_diff(&diff, None);
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].severity, Severity::Medium);
    assert!(findings[0].source.contains("unsafe-block"));
}

#[test]
fn sql_interpolation_detector_flags_formatted_sql() {
    let diff = make_diff(
        "src/db.rs",
        vec![
            (DiffLineKind::Ctx, "fn query_user(id: &str) {"),
            (
                DiffLineKind::Add,
                "    let q = format!(\"SELECT * FROM users WHERE id = '{}'\", id);",
            ),
            (DiffLineKind::Ctx, "}"),
        ],
    );
    let findings = scan_diff(&diff, None);
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].severity, Severity::High);
    assert!(findings[0].source.contains("sql-string-interpolation"));
}

#[test]
fn custom_rules_glob_and_regex_match() {
    let custom = vec![RuleConfig {
        id: "no-hack-in-code".into(),
        pattern: r"HACK|TEMP".into(),
        files: vec!["src/**/*.rs".into()],
        severity: Severity::Low,
        message: "Unresolved HACK in source code".into(),
    }];

    // Matched file
    let diff_match = make_diff(
        "src/parser.rs",
        vec![
            (DiffLineKind::Ctx, "fn parse() {"),
            (DiffLineKind::Add, "    // HACK: workaround for upstream bug"),
            (DiffLineKind::Ctx, "}"),
        ],
    );
    let findings_match = scan_diff(&diff_match, Some(&custom));
    assert_eq!(findings_match.len(), 1);
    assert_eq!(findings_match[0].source, "static:no-hack-in-code");

    // File excluded by glob
    let diff_nomatch = make_diff(
        "docs/notes.md",
        vec![(DiffLineKind::Add, "- HACK: workaround notes")],
    );
    let findings_nomatch = scan_diff(&diff_nomatch, Some(&custom));
    assert!(findings_nomatch.is_empty());
}

#[test]
fn slop_detector_catches_unimplemented_stubs() {
    let diff = make_diff(
        "src/service.rs",
        vec![
            (DiffLineKind::Add, "    todo!(\"implement process\");"),
            (DiffLineKind::Add, "    // TODO: implement caching"),
            (DiffLineKind::Add, "    unimplemented!();"),
        ],
    );
    let findings = scan_diff(&diff, None);
    assert_eq!(findings.len(), 3);
    for f in findings {
        assert_eq!(f.source, "static:slop-placeholder");
        assert_eq!(f.severity, Severity::Medium);
    }
}
