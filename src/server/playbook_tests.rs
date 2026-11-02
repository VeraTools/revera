use super::*;
use crate::playbook::{self, Playbook};
use std::collections::BTreeMap;

fn skill(name: &str, enabled: bool, body: &str) -> String {
    format!("---\nname: {name}\ndescription: Server checklist\nenabled: {enabled}\n---\n{body}")
}

async fn call(
    url: &str,
    cookie: &str,
    csrf: &str,
    method: &str,
    path: &str,
    content: Option<&str>,
) -> (u16, Value) {
    let mut request = reqwest::Client::new()
        .request(method.parse().unwrap(), format!("{url}{path}"))
        .header("Cookie", cookie)
        .header("X-Revera-CSRF", csrf);
    if let Some(content) = content {
        request = request.json(&json!({"content":content}));
    }
    let response = request.send().await.unwrap();
    let status = response.status().as_u16();
    (status, response.json().await.unwrap())
}

fn snapshot(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut files = BTreeMap::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries {
            let path = entry.unwrap().path();
            if path.is_dir() {
                files.extend(snapshot(&path));
            } else {
                files.insert(path.clone(), std::fs::read(path).unwrap());
            }
        }
    }
    files
}

#[tokio::test]
async fn playbook_roundtrip_builtins_overrides_custom_disable_and_review_identity() {
    let mock = MockServer::start().await;
    let (data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
    let (cookie, csrf) = administrator(&server).await;
    let (url, task) = http(server.clone()).await;
    let (status, builtin) = call(&url, &cookie, &csrf, "GET", "/api/playbook", None).await;
    assert_eq!(status, 200);
    assert!(builtin.get("error").is_none());
    assert!(!data.path().join("playbook").exists());
    let mut command = tokio::process::Command::new("revera");
    runner::review_playbook(&server, &mut command).unwrap();
    assert_eq!(command.as_std().get_args().count(), 0);
    assert_eq!(
        builtin["limits"],
        json!({"reviewer_max_bytes":playbook::REVIEWER_MAX_BYTES,"skill_max_bytes":playbook::SKILL_MAX_BYTES,"total_max_bytes":playbook::TOTAL_MAX_BYTES,"max_skills":playbook::MAX_SKILLS})
    );
    assert_eq!(
        builtin["reviewer"],
        json!({"builtin":playbook::BUILTIN_REVIEWER,"override":null,"effective":playbook::BUILTIN_REVIEWER})
    );
    assert_eq!(builtin["digest"], Playbook::builtin().digest);
    assert_eq!(builtin["total_bytes"], Playbook::builtin().total_bytes());
    for (actual, (name, text)) in builtin["skills"]
        .as_array()
        .unwrap()
        .iter()
        .zip(playbook::BUILTIN_SKILLS)
    {
        let parsed = playbook::validate_skill(name, text).unwrap();
        assert_eq!(
            *actual,
            json!({"name":name,"description":parsed.description,"builtin":text,"override":null,"effective":text,"enabled":true,"custom":false})
        );
    }
    assert_eq!(builtin["skills"].as_array().unwrap().len(), 12);
    let mut cfg = crate::config::Config::parse(CONFIG, "test").unwrap();
    let key =
        crate::state::review_key("base", "tree", "patch", &cfg.review_fingerprint("baseline"));
    let mut state = crate::state::ReviewState::default();
    state.record_outcome(
        "base",
        "head",
        "patch",
        &key,
        crate::report::RunStatus::Complete,
    );
    assert!(state.can_reuse(&key));
    let (status, changed) = call(
        &url,
        &cookie,
        &csrf,
        "PUT",
        "/api/playbook/reviewer",
        Some("Server method"),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(changed["reviewer"]["override"], "Server method");
    assert_eq!(changed["reviewer"]["effective"], "Server method");
    assert_ne!(changed["digest"], builtin["digest"]);
    assert_eq!(
        call(&url, &cookie, &csrf, "GET", "/api/playbook", None)
            .await
            .1,
        changed
    );
    let index_key = cfg.vera.index_key();
    cfg.playbook = Playbook::load(Some(&data.path().join("playbook"))).unwrap();
    assert_eq!(changed["digest"], cfg.playbook.digest);
    let changed_key =
        crate::state::review_key("base", "tree", "patch", &cfg.review_fingerprint("baseline"));
    assert!(!state.can_reuse(&changed_key));
    assert_eq!(index_key, cfg.vera.index_key());
    assert_eq!(
        call(
            &url,
            &cookie,
            &csrf,
            "PUT",
            "/api/playbook/reviewer",
            Some(playbook::BUILTIN_REVIEWER)
        )
        .await,
        (200, builtin.clone())
    );
    assert!(!data.path().join("playbook/reviewer.md").exists());
    let logic = playbook::BUILTIN_SKILLS[0].1;
    let override_text = format!("{logic}\nExtra check\n");
    let (status, edited) = call(
        &url,
        &cookie,
        &csrf,
        "PUT",
        "/api/playbook/skills/logic",
        Some(&override_text),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(edited["skills"][0]["override"], override_text);
    assert_eq!(edited["skills"][0]["effective"], override_text);
    assert_eq!(
        call(
            &url,
            &cookie,
            &csrf,
            "PUT",
            "/api/playbook/skills/logic",
            Some(logic)
        )
        .await,
        (200, builtin.clone())
    );
    assert!(!data.path().join("playbook/skills/logic.md").exists());
    let disabled = skill("logic", false, "Disabled check");
    let (status, edited) = call(
        &url,
        &cookie,
        &csrf,
        "PUT",
        "/api/playbook/skills/logic",
        Some(&disabled),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(edited["skills"][0]["enabled"], false);
    assert_eq!(edited["skills"][0]["effective"], disabled);
    assert!(
        !Playbook::load(Some(&data.path().join("playbook")))
            .unwrap()
            .names()
            .contains(&"logic")
    );
    for name in ["zebra", "alpha"] {
        let (status, edited) = call(
            &url,
            &cookie,
            &csrf,
            "PUT",
            &format!("/api/playbook/skills/{name}"),
            Some(&skill(name, true, "Custom check")),
        )
        .await;
        assert_eq!(status, 200);
        let custom = edited["skills"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["name"] == name)
            .unwrap();
        assert_eq!(custom["builtin"], Value::Null);
        assert_eq!(custom["custom"], true);
        assert_eq!(custom["effective"], custom["override"]);
    }
    let edited = call(&url, &cookie, &csrf, "GET", "/api/playbook", None)
        .await
        .1;
    assert_eq!(edited["skills"][12]["name"], "alpha");
    assert_eq!(edited["skills"][13]["name"], "zebra");
    assert_eq!(
        edited["total_bytes"],
        Playbook::load(Some(&data.path().join("playbook")))
            .unwrap()
            .total_bytes()
    );
    assert_eq!(
        call(
            &url,
            &cookie,
            &csrf,
            "DELETE",
            "/api/playbook/skills/missing",
            None
        )
        .await
        .0,
        404
    );
    for name in ["zebra", "alpha", "logic"] {
        assert_eq!(
            call(
                &url,
                &cookie,
                &csrf,
                "DELETE",
                &format!("/api/playbook/skills/{name}"),
                None
            )
            .await
            .0,
            200
        );
        assert!(
            !data
                .path()
                .join(format!("playbook/skills/{name}.md"))
                .exists()
        );
    }
    assert_eq!(
        call(
            &url,
            &cookie,
            &csrf,
            "PUT",
            "/api/playbook/reviewer",
            Some("Temporary method")
        )
        .await
        .0,
        200
    );
    assert_eq!(
        call(
            &url,
            &cookie,
            &csrf,
            "DELETE",
            "/api/playbook/reviewer",
            None
        )
        .await,
        (200, builtin)
    );
    task.abort();
}

#[tokio::test]
async fn playbook_auth_validation_and_secret_errors_never_change_stored_files() {
    let mock = MockServer::start().await;
    let (data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
    let (cookie, csrf) = administrator(&server).await;
    let (url, task) = http(server).await;
    assert_eq!(
        reqwest::Client::new()
            .get(format!("{url}/api/playbook"))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        reqwest::Client::new()
            .put(format!("{url}/api/playbook/reviewer"))
            .header("Cookie", &cookie)
            .json(&json!({"content":"Denied"}))
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert!(!data.path().join("playbook").exists());
    assert_eq!(
        call(
            &url,
            &cookie,
            &csrf,
            "PUT",
            "/api/playbook/reviewer",
            Some("Keep this")
        )
        .await
        .0,
        200
    );
    let secret = "server-playbook-registered-secret-728401";
    crate::redact::register(secret);
    let before = snapshot(&data.path().join("playbook"));
    for (path, content, message) in [
        (
            "/api/playbook/reviewer",
            "x".repeat(playbook::REVIEWER_MAX_BYTES + 1),
            "exceeds",
        ),
        (
            "/api/playbook/skills/logic",
            skill("logic", true, &"x".repeat(playbook::SKILL_MAX_BYTES)),
            "exceeds",
        ),
        (
            "/api/playbook/skills/logic",
            skill("other", true, "bad name"),
            "file stem",
        ),
        (
            "/api/playbook/skills/Bad",
            skill("Bad", true, "bad path"),
            "invalid skill name",
        ),
        (
            "/api/playbook/skills/logic",
            "not front matter".into(),
            "front matter",
        ),
        (
            "/api/playbook/reviewer",
            format!("Do not store {secret}"),
            "content contains a registered secret",
        ),
        (
            "/api/playbook/skills/logic",
            skill("logic", true, secret),
            "content contains a registered secret",
        ),
    ] {
        let (status, error) = call(&url, &cookie, &csrf, "PUT", path, Some(&content)).await;
        assert_eq!(status, 400, "{path}: {error}");
        assert!(
            error["error"].as_str().unwrap().contains(message),
            "{error}"
        );
        assert!(!error.to_string().contains(secret));
        assert_eq!(snapshot(&data.path().join("playbook")), before);
    }
    let response = reqwest::Client::new()
        .put(format!("{url}/api/playbook/reviewer"))
        .header("Cookie", &cookie)
        .header("X-Revera-CSRF", &csrf)
        .json(&json!({"content":false}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    assert!(response.json::<Value>().await.unwrap()["error"].is_string());
    assert_eq!(snapshot(&data.path().join("playbook")), before);
    task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn playbook_total_and_concurrent_skill_count_limits_validate_before_writing() {
    let mock = MockServer::start().await;
    let (data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
    let (cookie, csrf) = administrator(&server).await;
    let (url, task) = http(server).await;
    let dir = data.path().join("playbook");
    let mut rejected = false;
    for (name, _) in playbook::BUILTIN_SKILLS {
        let before = snapshot(&dir);
        let (status, error) = call(
            &url,
            &cookie,
            &csrf,
            "PUT",
            &format!("/api/playbook/skills/{name}"),
            Some(&skill(name, true, &"x".repeat(7000))),
        )
        .await;
        if status == 400 {
            assert!(
                error["error"]
                    .as_str()
                    .unwrap()
                    .contains("effective playbook exceeds")
            );
            assert_eq!(snapshot(&dir), before);
            rejected = true;
            break;
        }
        assert_eq!(status, 200);
    }
    assert!(rejected, "total limit was not exercised");
    // Remove only this fixture's playbook before testing a separate limit.
    std::fs::remove_dir_all(&dir).unwrap();
    for i in 0..11 {
        let name = format!("custom-{i}");
        assert_eq!(
            call(
                &url,
                &cookie,
                &csrf,
                "PUT",
                &format!("/api/playbook/skills/{name}"),
                Some(&skill(&name, false, "disabled still counts"))
            )
            .await
            .0,
            200
        );
    }
    let last_a = skill("last-a", false, "A");
    let last_b = skill("last-b", false, "B");
    let (a, b) = tokio::join!(
        call(
            &url,
            &cookie,
            &csrf,
            "PUT",
            "/api/playbook/skills/last-a",
            Some(&last_a)
        ),
        call(
            &url,
            &cookie,
            &csrf,
            "PUT",
            "/api/playbook/skills/last-b",
            Some(&last_b)
        ),
    );
    let mut statuses = [a.0, b.0];
    statuses.sort();
    assert_eq!(statuses, [200, 400]);
    assert_eq!(Playbook::load(Some(&dir)).unwrap().skills.len(), 24);
    assert_ne!(
        dir.join("skills/last-a.md").exists(),
        dir.join("skills/last-b.md").exists()
    );
    assert_eq!(snapshot(&dir).len(), 12);
    task.abort();
}

#[tokio::test]
async fn resetting_an_override_validates_restored_builtin_before_removing_files() {
    let mock = MockServer::start().await;
    let (data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
    let (cookie, csrf) = administrator(&server).await;
    let (url, task) = http(server).await;
    for (name, _) in playbook::BUILTIN_SKILLS {
        crate::fsutil::write_repo_file(
            data.path(),
            &Path::new("playbook/skills").join(format!("{name}.md")),
            skill(name, false, "Disabled").as_bytes(),
        )
        .unwrap();
    }
    let mut used = 0;
    for i in 0..8 {
        let name = format!("custom-{i}");
        let text = skill(&name, true, &"x".repeat(7000));
        let parsed = playbook::validate_skill(&name, &text).unwrap();
        used += parsed.name.len() + parsed.description.len() + parsed.body.len();
        crate::fsutil::write_repo_file(
            data.path(),
            &Path::new("playbook/skills").join(format!("{name}.md")),
            text.as_bytes(),
        )
        .unwrap();
    }
    let builtin_logic = playbook::validate_skill("logic", playbook::BUILTIN_SKILLS[0].1).unwrap();
    let restored =
        builtin_logic.name.len() + builtin_logic.description.len() + builtin_logic.body.len();
    let reviewer = "x".repeat(playbook::TOTAL_MAX_BYTES - restored + 1 - used);
    assert!(reviewer.len() <= playbook::REVIEWER_MAX_BYTES);
    crate::fsutil::write_repo_file(
        data.path(),
        Path::new("playbook/reviewer.md"),
        reviewer.as_bytes(),
    )
    .unwrap();
    let dir = data.path().join("playbook");
    Playbook::load(Some(&dir)).unwrap();
    let before = snapshot(&dir);
    for (method, content) in [
        ("DELETE", None),
        ("PUT", Some(playbook::BUILTIN_SKILLS[0].1)),
    ] {
        let (status, error) = call(
            &url,
            &cookie,
            &csrf,
            method,
            "/api/playbook/skills/logic",
            content,
        )
        .await;
        assert_eq!(status, 400);
        assert!(
            error["error"]
                .as_str()
                .unwrap()
                .contains("effective playbook exceeds")
        );
        assert_eq!(snapshot(&dir), before);
    }
    task.abort();
}

#[cfg(unix)]
#[tokio::test]
async fn playbook_mutations_refuse_symlinked_storage() {
    use std::os::unix::fs::symlink;
    let mock = MockServer::start().await;
    let (data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
    let (cookie, csrf) = administrator(&server).await;
    let (url, task) = http(server.clone()).await;
    let external = tempfile::tempdir().unwrap();
    let dir = data.path().join("playbook");
    symlink(external.path(), &dir).unwrap();
    let mut command = tokio::process::Command::new("revera");
    let error = runner::review_playbook(&server, &mut command).unwrap_err();
    assert!(error.to_string().contains("real directory"));
    assert_eq!(command.as_std().get_args().count(), 0);
    assert_eq!(
        call(
            &url,
            &cookie,
            &csrf,
            "PUT",
            "/api/playbook/reviewer",
            Some("Do not escape")
        )
        .await
        .0,
        400
    );
    assert!(!external.path().join("reviewer.md").exists());
    std::fs::remove_file(&dir).unwrap();
    crate::fsutil::write_repo_file(data.path(), Path::new("playbook"), b"not a directory").unwrap();
    assert!(runner::review_playbook(&server, &mut command).is_err());
    std::fs::remove_file(&dir).unwrap();
    crate::fsutil::ensure_dir(data.path(), Path::new("playbook")).unwrap();
    symlink(external.path(), dir.join("skills")).unwrap();
    assert_eq!(
        call(
            &url,
            &cookie,
            &csrf,
            "PUT",
            "/api/playbook/skills/logic",
            Some(&skill("logic", true, "Do not escape"))
        )
        .await
        .0,
        400
    );
    assert!(!external.path().join("logic.md").exists());
    std::fs::remove_file(dir.join("skills")).unwrap();
    crate::fsutil::write_atomic_in(external.path(), "reviewer.md", b"Do not replace").unwrap();
    symlink(external.path().join("reviewer.md"), dir.join("reviewer.md")).unwrap();
    for method in ["PUT", "DELETE"] {
        assert_eq!(
            call(
                &url,
                &cookie,
                &csrf,
                method,
                "/api/playbook/reviewer",
                (method == "PUT").then_some("Do not follow")
            )
            .await
            .0,
            500
        );
        assert_eq!(
            std::fs::read_to_string(external.path().join("reviewer.md")).unwrap(),
            "Do not replace"
        );
    }
    task.abort();
}

#[tokio::test]
async fn bad_disk_playbook_is_visible_and_repairable_but_reviews_fail_clearly() {
    let mock = MockServer::start().await;
    let (data, server) = server(&mock, runner::Runner::current().unwrap(), true).await;
    let (cookie, csrf) = administrator(&server).await;
    let (url, task) = http(server.clone()).await;
    crate::fsutil::write_repo_file(
        data.path(),
        Path::new("playbook/skills/logic.md"),
        b"broken front matter",
    )
    .unwrap();
    let (status, book) = call(&url, &cookie, &csrf, "GET", "/api/playbook", None).await;
    assert_eq!(status, 200);
    assert!(book["error"].as_str().unwrap().contains("logic.md"));
    assert_eq!(book["skills"].as_array().unwrap().len(), 12);
    assert_eq!(book["skills"][0]["builtin"], playbook::BUILTIN_SKILLS[0].1);
    assert_eq!(book["skills"][0]["override"], "broken front matter");
    let mut command = tokio::process::Command::new("revera");
    let error = runner::review_playbook(&server, &mut command).unwrap_err();
    assert!(format!("{error:#}").contains("logic.md"));
    assert!(format!("{error:#}").contains("load server review playbook"));
    let (status, book) = call(
        &url,
        &cookie,
        &csrf,
        "DELETE",
        "/api/playbook/skills/logic",
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert!(book.get("error").is_none());
    // Replacing an over-size broken target also repairs it without reading it.
    crate::fsutil::write_repo_file(
        data.path(),
        Path::new("playbook/reviewer.md"),
        "x".repeat(playbook::REVIEWER_MAX_BYTES + 1).as_bytes(),
    )
    .unwrap();
    assert!(
        call(&url, &cookie, &csrf, "GET", "/api/playbook", None)
            .await
            .1["error"]
            .is_string()
    );
    assert_eq!(
        call(
            &url,
            &cookie,
            &csrf,
            "PUT",
            "/api/playbook/reviewer",
            Some("Repaired method")
        )
        .await
        .0,
        200
    );
    let mut command = tokio::process::Command::new("revera");
    runner::review_playbook(&server, &mut command).unwrap();
    assert_eq!(
        command.as_std().get_args().collect::<Vec<_>>(),
        [
            std::ffi::OsStr::new("--playbook-dir"),
            data.path().join("playbook").as_os_str()
        ]
    );
    task.abort();
}
