//! Operator-owned review method and defect checklists, independent of the index.
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;

pub const REVIEWER_MAX_BYTES: usize = 16 * 1024;
pub const SKILL_MAX_BYTES: usize = 8 * 1024;
pub const TOTAL_MAX_BYTES: usize = 64 * 1024;
pub const MAX_SKILLS: usize = 24;

pub const BUILTIN_REVIEWER: &str = include_str!("../playbook/reviewer.md");
pub const BUILTIN_SKILLS: &[(&str, &str)] = &[
    ("logic", include_str!("../playbook/skills/logic.md")),
    ("contracts", include_str!("../playbook/skills/contracts.md")),
    ("state", include_str!("../playbook/skills/state.md")),
    ("errors", include_str!("../playbook/skills/errors.md")),
    (
        "concurrency",
        include_str!("../playbook/skills/concurrency.md"),
    ),
    ("security", include_str!("../playbook/skills/security.md")),
    (
        "filesystem",
        include_str!("../playbook/skills/filesystem.md"),
    ),
    (
        "subprocess",
        include_str!("../playbook/skills/subprocess.md"),
    ),
    ("resources", include_str!("../playbook/skills/resources.md")),
    ("config", include_str!("../playbook/skills/config.md")),
    ("tests", include_str!("../playbook/skills/tests.md")),
    ("docs", include_str!("../playbook/skills/docs.md")),
];

#[derive(Debug, Clone)]
pub struct Playbook {
    pub reviewer: String,
    pub skills: Vec<Skill>,
    pub digest: String,
}

#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub body: String,
    pub enabled: bool,
    pub origin: Origin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Builtin,
    Override,
    Custom,
}

/// Raw operator files, also used to validate dashboard edits before writing.
#[derive(Default)]
pub(crate) struct Overrides {
    pub reviewer: Option<String>,
    pub skills: BTreeMap<String, String>,
}

impl Overrides {
    /// Omit the file being replaced/deleted so a malformed override is repairable.
    pub fn load(dir: &Path, omit: Option<&Path>) -> Result<Self> {
        let dir = dir
            .canonicalize()
            .with_context(|| format!("resolve playbook {}", dir.display()))?;
        real_dir(&dir)?;
        let mut files = Self::default();
        for path in entries(&dir)? {
            match path.file_name().and_then(|s| s.to_str()) {
                Some("reviewer.md") if omit == Some(Path::new("reviewer.md")) => {}
                Some("reviewer.md") => files.reviewer = Some(read_text(&path, REVIEWER_MAX_BYTES)?),
                Some("skills") => {
                    real_dir(&path)?;
                    for path in entries(&path)? {
                        if omit.is_some_and(|rel| path == dir.join(rel)) {
                            continue;
                        }
                        if ignored(&path)? {
                            continue;
                        }
                        let name = path
                            .file_name()
                            .and_then(|s| s.to_str())
                            .and_then(|s| s.strip_suffix(".md"))
                            .filter(|s| valid_name(s))
                            .with_context(|| {
                                format!("{}: expected <skill-name>.md", path.display())
                            })?;
                        files
                            .skills
                            .insert(name.into(), read_text(&path, SKILL_MAX_BYTES)?);
                    }
                }
                _ if ignored(&path)? => {}
                _ => bail!("{}: unexpected playbook entry", path.display()),
            }
        }
        Ok(files)
    }
}

pub(crate) fn valid_name(name: &str) -> bool {
    (1..=40).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && name.as_bytes()[0] != b'-'
}

/// Pure validation, also used by the operator API before writing a skill.
pub fn validate_skill(name: &str, text: &str) -> Result<Skill> {
    if !valid_name(name) {
        bail!("{name}.md: name must match ^[a-z0-9][a-z0-9-]{{0,39}}$");
    }
    if text.len() > SKILL_MAX_BYTES {
        bail!("{name}.md: skill exceeds {SKILL_MAX_BYTES} bytes");
    }
    let parse = || -> Result<Skill> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Header {
            name: String,
            description: String,
            #[serde(default = "enabled")]
            enabled: bool,
        }
        fn enabled() -> bool {
            true
        }
        let mut lines = text.split_inclusive('\n');
        if lines.next().map(str::trim_end) != Some("---") {
            bail!("expected opening --- front matter line");
        }
        let mut header = String::new();
        let mut closed = false;
        for line in lines.by_ref() {
            if line.trim_end() == "---" {
                closed = true;
                break;
            }
            header.push_str(line);
        }
        if !closed {
            bail!("missing closing --- front matter line");
        }
        let h: Header = serde_saphyr::from_str(&header).context("invalid front matter")?;
        if h.name != name {
            bail!("front matter name must equal file stem {name:?}");
        }
        if h.description.trim().is_empty()
            || h.description.contains(['\n', '\r'])
            || h.description.chars().count() > 300
        {
            bail!("description must be one line, 1–300 characters");
        }
        Ok(Skill {
            name: h.name,
            description: h.description,
            body: lines.collect(),
            enabled: h.enabled,
            origin: Origin::Custom,
        })
    };
    parse().with_context(|| format!("{name}.md"))
}

/// Pure validation, also used by the operator API before writing reviewer.md.
pub fn validate_reviewer(text: &str) -> Result<()> {
    if text.len() > REVIEWER_MAX_BYTES {
        bail!("reviewer.md exceeds {REVIEWER_MAX_BYTES} bytes");
    }
    Ok(())
}

fn real_dir(path: &Path) -> Result<()> {
    let m = std::fs::symlink_metadata(path).with_context(|| format!("stat {}", path.display()))?;
    if m.file_type().is_symlink() || !m.is_dir() {
        bail!("{} must be a real directory, not a symlink", path.display());
    }
    Ok(())
}

fn read_text(path: &Path, limit: usize) -> Result<String> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Do not follow a swapped-in symlink or block opening a swapped-in FIFO.
        options.custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK);
    }
    #[cfg(not(unix))]
    {
        let m =
            std::fs::symlink_metadata(path).with_context(|| format!("stat {}", path.display()))?;
        if m.file_type().is_symlink() {
            bail!("{} must be a regular file, not a symlink", path.display());
        }
    }
    let file = options
        .open(path)
        .with_context(|| format!("open {} without following symlinks", path.display()))?;
    let m = file
        .metadata()
        .with_context(|| format!("stat opened {}", path.display()))?;
    if !m.is_file() {
        bail!("{} must be a regular file, not a symlink", path.display());
    }
    if m.len() > limit as u64 {
        bail!("{} exceeds {limit} bytes", path.display());
    }
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        bail!("{} exceeds {limit} bytes", path.display());
    }
    String::from_utf8(bytes).with_context(|| format!("{} is not UTF-8", path.display()))
}

fn entries(path: &Path) -> Result<Vec<std::path::PathBuf>> {
    let mut entries = std::fs::read_dir(path)
        .with_context(|| format!("read {}", path.display()))?
        .map(|e| e.map(|e| e.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    entries.sort();
    Ok(entries)
}

fn ignored(path: &Path) -> Result<bool> {
    let m = std::fs::symlink_metadata(path)?;
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .with_context(|| format!("{} has a non-UTF-8 name", path.display()))?;
    if m.file_type().is_symlink() || !m.is_file() {
        bail!("{} must be a regular file, not a symlink", path.display());
    }
    Ok(name.starts_with('.') && !name.ends_with(".md"))
}

impl Default for Playbook {
    fn default() -> Self {
        Self::builtin()
    }
}

impl Playbook {
    pub fn builtin() -> Self {
        let mut p = Self {
            reviewer: BUILTIN_REVIEWER.into(),
            skills: BUILTIN_SKILLS
                .iter()
                .map(|(name, text)| {
                    let mut skill = validate_skill(name, text).expect("built-in skill must parse");
                    skill.origin = Origin::Builtin;
                    skill
                })
                .collect(),
            digest: String::new(),
        };
        p.finalize().expect("built-in playbook must fit limits");
        p
    }

    pub fn load(dir: Option<&Path>) -> Result<Self> {
        match dir {
            Some(dir) => Self::from_overrides(&Overrides::load(dir, None)?)
                .with_context(|| format!("playbook {}", dir.display())),
            None => Ok(Self::builtin()),
        }
    }

    pub(crate) fn from_overrides(files: &Overrides) -> Result<Self> {
        let mut p = Self::builtin();
        if let Some(text) = &files.reviewer {
            validate_reviewer(text)?;
            p.reviewer.clone_from(text);
        }
        for (name, text) in &files.skills {
            let mut skill = validate_skill(name, text)?;
            if let Some(old) = p.skills.iter_mut().find(|s| s.name == *name) {
                skill.origin = Origin::Override;
                *old = skill;
            } else {
                p.skills.push(skill);
            }
        }
        p.finalize()?;
        Ok(p)
    }

    /// Bytes covered by the effective-content limit (disabled skills excluded).
    pub fn total_bytes(&self) -> usize {
        self.reviewer.len()
            + self
                .skills
                .iter()
                .filter(|s| s.enabled)
                .map(|s| s.name.len() + s.description.len() + s.body.len())
                .sum::<usize>()
    }

    fn finalize(&mut self) -> Result<()> {
        validate_reviewer(&self.reviewer)?;
        if self.skills.len() > MAX_SKILLS {
            bail!("at most {MAX_SKILLS} skills are allowed");
        }
        let mut h = Sha256::new();
        h.update(self.reviewer.as_bytes());
        h.update([0]);
        for s in self.skills.iter().filter(|s| s.enabled) {
            for part in [&s.name, &s.description, &s.body] {
                h.update(part.as_bytes());
                h.update([0]);
            }
            h.update([u8::from(s.enabled)]);
        }
        if self.total_bytes() > TOTAL_MAX_BYTES {
            bail!("effective playbook exceeds {TOTAL_MAX_BYTES} bytes");
        }
        self.digest = hex::encode(&h.finalize()[..8]);
        Ok(())
    }

    pub fn skill(&self, name: &str) -> Option<&Skill> {
        self.skills.iter().find(|s| s.enabled && s.name == name)
    }

    pub fn names(&self) -> Vec<&str> {
        self.skills
            .iter()
            .filter(|s| s.enabled)
            .map(|s| s.name.as_str())
            .collect()
    }

    pub fn normalize_category(&self, category: &str) -> String {
        let name = category.trim().to_lowercase();
        if self.skill(&name).is_some() {
            name
        } else {
            "other".into()
        }
    }

    pub fn classes_block(&self) -> String {
        let mut out = String::from("## Defect classes\n\n");
        for s in self.skills.iter().filter(|s| s.enabled) {
            out.push_str(&format!(
                "### {}\n{}\n\n{}\n\n",
                s.name, s.description, s.body
            ));
        }
        crate::redact::text(&out).into_owned()
    }

    pub fn investigator_block(&self) -> String {
        crate::redact::text(&format!(
            "\n\n# Reviewer playbook\n\n{}\n\n{}",
            self.reviewer,
            self.classes_block()
        ))
        .into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skill(name: &str, enabled: bool, body: &str) -> String {
        format!("---\nname: {name}\ndescription: A checklist\nenabled: {enabled}\n---\n{body}")
    }
    fn write(dir: &Path, name: &str, text: &str) {
        crate::fsutil::write_repo_file(dir, Path::new(name), text.as_bytes()).unwrap();
    }

    #[test]
    fn builtin_parses_and_renders() {
        let p = Playbook::builtin();
        assert_eq!(p.skills.len(), 12);
        assert_eq!(p.digest.len(), 16);
        assert!(
            p.skills
                .iter()
                .all(|s| s.enabled && s.origin == Origin::Builtin)
        );
        assert!(p.investigator_block().contains("# Reviewer playbook"));
        assert!(p.classes_block().contains("### contracts"));
        assert_eq!(p.digest, Playbook::load(None).unwrap().digest);
    }

    #[test]
    fn overrides_custom_disabled_and_digest() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "reviewer.md", "Operator method");
        write(
            d.path(),
            "skills/logic.md",
            &skill("logic", true, "Replacement"),
        );
        write(
            d.path(),
            "skills/errors.md",
            &skill("errors", false, "Disabled"),
        );
        write(
            d.path(),
            "skills/custom.md",
            &skill("custom", true, "Custom"),
        );
        write(d.path(), "skills/.logic.md.tmp-1", "incomplete");
        let p = Playbook::load(Some(d.path())).unwrap();
        assert_eq!(p.skill("logic").unwrap().origin, Origin::Override);
        assert_eq!(p.skill("custom").unwrap().origin, Origin::Custom);
        assert!(p.skill("errors").is_none());
        assert!(!p.classes_block().contains("Disabled"));
        assert_ne!(p.digest, Playbook::builtin().digest);
        assert_eq!(p.digest, Playbook::load(Some(d.path())).unwrap().digest);
        write(
            d.path(),
            "skills/errors.md",
            &skill("errors", false, "Another disabled body"),
        );
        assert_eq!(p.digest, Playbook::load(Some(d.path())).unwrap().digest);
        write(
            d.path(),
            "skills/logic.md",
            &skill("logic", true, "Changed"),
        );
        assert_ne!(p.digest, Playbook::load(Some(d.path())).unwrap().digest);
    }

    #[test]
    fn limits() {
        assert!(validate_reviewer(&"x".repeat(REVIEWER_MAX_BYTES)).is_ok());
        assert!(validate_reviewer(&"x".repeat(REVIEWER_MAX_BYTES + 1)).is_err());
        let mut s = skill("logic", true, "");
        s.push_str(&"x".repeat(SKILL_MAX_BYTES - s.len()));
        assert!(validate_skill("logic", &s).is_ok());
        s.push('x');
        assert!(validate_skill("logic", &s).is_err());
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "reviewer.md", &"x".repeat(REVIEWER_MAX_BYTES + 1));
        assert!(
            format!("{:#}", Playbook::load(Some(d.path())).unwrap_err()).contains("reviewer.md")
        );
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "skills/logic.md", &s);
        assert!(format!("{:#}", Playbook::load(Some(d.path())).unwrap_err()).contains("logic.md"));
        let d = tempfile::tempdir().unwrap();
        for i in 0..13 {
            write(
                d.path(),
                &format!("skills/custom{i}.md"),
                &skill(&format!("custom{i}"), false, ""),
            );
        }
        assert!(
            Playbook::load(Some(d.path()))
                .unwrap_err()
                .to_string()
                .contains("playbook")
        );
        let d = tempfile::tempdir().unwrap();
        for (name, _) in BUILTIN_SKILLS {
            write(
                d.path(),
                &format!("skills/{name}.md"),
                &skill(name, true, &"x".repeat(7000)),
            );
        }
        assert!(
            format!("{:#}", Playbook::load(Some(d.path())).unwrap_err())
                .contains("effective playbook")
        );
    }

    #[test]
    fn invalid_files_and_front_matter() {
        for name in ["", "Logic", "../logic", "-logic", &"a".repeat(41)] {
            assert!(validate_skill(name, &skill(name, true, "body")).is_err());
        }
        for text in [
            "body".into(),
            "---\nname: logic\n".into(),
            skill("other", true, ""),
            "---\nname: logic\ndescription: a\nunknown: x\n---\n".into(),
            "---\nname: logic\n---\n".into(),
            "---\nname: logic\ndescription: a\nenabled: maybe\n---\n".into(),
            format!("---\nname: logic\ndescription: {}\n---\n", "x".repeat(301)),
            "---\nname: logic\ndescription: ''\n---\n".into(),
            "---\nname: logic\ndescription: '   '\n---\n".into(),
            "---\nname: logic\ndescription: |\n  a\n  b\n---\n".into(),
        ] {
            assert!(validate_skill("logic", &text).is_err(), "{text}");
        }
        for name in [
            "unexpected.txt",
            "other.md",
            "skills/Bad.md",
            "skills/oops.txt",
            "skills/.hidden.md",
        ] {
            let d = tempfile::tempdir().unwrap();
            write(d.path(), name, "text");
            assert!(format!("{:#}", Playbook::load(Some(d.path())).unwrap_err()).contains(name));
        }
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "reviewer.md", "valid");
        std::fs::write(d.path().join("reviewer.md"), [0xff]).unwrap();
        assert!(format!("{:#}", Playbook::load(Some(d.path())).unwrap_err()).contains("UTF-8"));
    }

    #[cfg(unix)]
    #[test]
    fn accepts_operator_symlinked_root_and_ancestors() {
        use std::os::unix::fs::symlink;
        let d = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        write(target.path(), "playbook/reviewer.md", "Operator method");
        symlink(target.path(), d.path().join("ancestor")).unwrap();
        symlink(target.path().join("playbook"), d.path().join("root")).unwrap();
        for path in [d.path().join("ancestor/playbook"), d.path().join("root")] {
            assert_eq!(
                Playbook::load(Some(&path)).unwrap().reviewer,
                "Operator method"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinks_and_non_regular_entries() {
        use std::os::unix::fs::symlink;
        let d = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        write(target.path(), "reviewer.md", "Do not follow this file");
        std::fs::create_dir(target.path().join("skills")).unwrap();
        write(
            target.path(),
            "skills/logic.md",
            &skill("logic", true, "External checklist"),
        );
        for name in ["reviewer.md", "skills", ".ignored"] {
            let path = d.path().join(name);
            symlink(
                target.path().join(if name == "skills" {
                    "skills"
                } else {
                    "reviewer.md"
                }),
                &path,
            )
            .unwrap();
            assert!(Playbook::load(Some(d.path())).is_err());
            std::fs::remove_file(path).unwrap();
        }
        std::fs::create_dir(d.path().join("reviewer.md")).unwrap();
        assert!(Playbook::load(Some(d.path())).is_err());
        std::fs::remove_dir(d.path().join("reviewer.md")).unwrap();
        std::fs::create_dir(d.path().join("skills")).unwrap();
        symlink(
            target.path().join("skills/logic.md"),
            d.path().join("skills/logic.md"),
        )
        .unwrap();
        assert!(format!("{:#}", Playbook::load(Some(d.path())).unwrap_err()).contains("logic.md"));
    }
}
