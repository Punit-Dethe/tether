//! M5's bar: tagged upstream releases are tracked, and security fixes are
//! found by their GHSA/CVE references, released or not.

mod support;

use std::fs;

use serde_json::Value;
use support::{Fixture, Repo, tether, tether_with};

const AGENT: &[(&str, &str)] = &[("TETHER_ACTOR", "agent")];

struct World {
    _fx: Fixture,
    fork: Repo,
    fork_tag: String,
    xss: String,
    rc: String,
    cve: String,
}

fn world(extra_config: &str) -> World {
    let fx = Fixture::downloaded_fork();
    let root = fx.upstream.path.parent().unwrap().to_path_buf();
    let upstream = Repo::init(root.join("up-rel"));
    upstream.write("app.txt", "v1.0\n");
    let base = upstream.commit("Release 1.0");
    upstream.git(&["tag", "-a", "v1.0.0", "-m", "1.0.0"]);

    upstream.write("preview.txt", "escaped\n");
    let xss = upstream
        .commit("Fix XSS in the preview (#3)\n\nReported privately; see GHSA-abcd-1234-wxyz.");
    upstream.git(&["tag", "v1.1.0"]);

    upstream.write("app.txt", "v1.2 candidate\n");
    let rc = upstream.commit("Prepare 1.2 (#4)");
    upstream.git(&["tag", "v1.2.0-rc.1"]);

    upstream.write("auth.txt", "checked\n");
    let cve = upstream.commit("Check tokens on refresh (#9)\n\nFixes CVE-2026-12345.");

    let fork = Repo::init(root.join("fork-rel"));
    fork.write("app.txt", "v1.0\n");
    fork.write("fork.txt", "ours\n");
    fork.commit(&format!("Initial commit\n\nUpstream-Synced: {base}"));
    // The fork's own release line uses the same tag names as upstream's.
    fork.git(&["tag", "v1.0.0"]);
    let fork_tag = fork.git(&["rev-parse", "v1.0.0"]);
    let url = upstream.path.to_string_lossy().replace('\\', "/");
    fork.git(&["remote", "add", "upstream", &url]);
    fs::create_dir_all(fork.path.join(".tether")).unwrap();
    fork.write(
        ".tether/config.toml",
        &format!("[upstream]\nname = \"Up\"\ntrack = \"releases\"\n{extra_config}"),
    );
    fork.commit("add tether config");
    // A dry run fetches the branch and the release tags.
    let dry = tether(&fork.path, &["sync", "--dry-run"]);
    assert!(dry.code != 1, "{}", dry.stdout);
    World {
        _fx: fx,
        fork,
        fork_tag,
        xss,
        rc,
        cve,
    }
}

fn item<'a>(inbox: &'a Value, sha: &str) -> &'a Value {
    inbox["data"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["sha"] == sha)
        .unwrap_or_else(|| panic!("{sha} not in the inbox"))
}

#[test]
fn the_newest_release_is_the_target_and_the_rest_is_unreleased() {
    let w = world("");
    let inbox = tether(&w.fork.path, &["inbox", "--all"]);
    assert_eq!(inbox.code, 0, "{}", inbox.stdout);
    // v1.2.0-rc.1 is a pre-release: the target is v1.1.0.
    assert_eq!(inbox.json["data"]["release"], "v1.1.0");
    assert_eq!(inbox.json["data"]["target"], w.xss.as_str());
    assert_eq!(inbox.json["data"]["behind"], 1);
    assert_eq!(inbox.json["data"]["unreleased"], 2);
    assert_eq!(item(&inbox.json, &w.xss)["state"], "pending");
    assert_eq!(item(&inbox.json, &w.xss)["release"], "v1.1.0");
    assert_eq!(item(&inbox.json, &w.rc)["state"], "unreleased");
    assert_eq!(item(&inbox.json, &w.cve)["state"], "unreleased");
    // Upstream's tags never touch the fork's own.
    assert_eq!(w.fork.git(&["rev-parse", "v1.0.0"]), w.fork_tag);
}

#[test]
fn security_fixes_are_found_by_advisory_ids_and_keywords() {
    let w = world("");
    let inbox = tether(&w.fork.path, &["inbox", "--security"]);
    assert_eq!(inbox.code, 0, "{}", inbox.stdout);
    let shas: Vec<&str> = inbox.json["data"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["sha"].as_str().unwrap())
        .collect();
    assert_eq!(shas, vec![w.cve.as_str(), w.xss.as_str()]);
    let xss = &item(&inbox.json, &w.xss)["security"];
    assert_eq!(xss[0], "GHSA-ABCD-1234-WXYZ");
    assert_eq!(xss[1], "keyword: xss");
    assert_eq!(item(&inbox.json, &w.cve)["security"][0], "CVE-2026-12345");
    assert!(
        inbox.json["summary"]
            .as_str()
            .unwrap()
            .starts_with("2 possible security fix(es)"),
        "{}",
        inbox.stdout
    );
}

#[test]
fn an_unreleased_security_fix_is_a_persons_call() {
    let w = world("");
    let fork = &w.fork;
    let status = tether_with(&fork.path, &["status"], AGENT);
    assert_eq!(status.json["data"]["phase"], "behind", "{}", status.stdout);
    assert!(
        status.json["summary"]
            .as_str()
            .unwrap()
            .starts_with("1 pending security fix(es)"),
        "{}",
        status.stdout
    );

    // Syncing to the release is the agent's to do.
    let sync = tether_with(&fork.path, &["sync"], AGENT);
    assert_eq!(sync.code, 2, "{}", sync.stdout);
    assert_eq!(sync.json["data"]["target"], w.xss.as_str());
    let done = tether_with(&fork.path, &["sync", "--continue"], AGENT);
    assert_eq!(done.code, 0, "{}", done.stdout);

    // Past the release only the unreleased CVE fix is left: a person decides.
    let status = tether_with(&fork.path, &["status"], AGENT);
    assert_eq!(
        status.json["data"]["phase"], "security",
        "{}",
        status.stdout
    );
    assert_eq!(status.json["next"]["kind"], "ask_human");
    let command = status.json["next"]["command"].as_str().unwrap().to_owned();
    assert_eq!(command, format!("tether sync --target {}", &w.cve[..12]));
    let args: Vec<&str> = command.split(' ').skip(1).collect();
    let agent = tether_with(&fork.path, &args, AGENT);
    assert_eq!(
        agent.json["error"]["code"], "needs_human",
        "{}",
        agent.stdout
    );
    let person = tether(&fork.path, &args);
    assert_eq!(person.code, 2, "{}", person.stdout);
    assert_eq!(person.json["data"]["target"], w.cve.as_str());
}

#[test]
fn branch_mode_still_labels_each_commit_with_its_release() {
    let fx = Fixture::downloaded_fork();
    let first = fx.upstream.git(&["rev-parse", "HEAD"]);
    fx.upstream.write("a.txt", "a\n");
    let shipped = fx.upstream.commit("Ship a (#1)");
    fx.upstream.git(&["tag", "-a", "v1.0.0", "-m", "1.0.0"]);
    fx.upstream.write("b.txt", "b\n");
    let later = fx.upstream.commit("Add b (#2)");
    fx.fork.git(&["tag", "v1.0.0"]);
    let fork_tag = fx.fork.git(&["rev-parse", "v1.0.0"]);
    let init = tether(&fx.fork.path, &["init", "--sync-point", &first]);
    assert_eq!(init.code, 0, "{}", init.stdout);
    let dry = tether(&fx.fork.path, &["sync", "--dry-run"]);
    assert!(dry.code != 1, "{}", dry.stdout);

    let inbox = tether(&fx.fork.path, &["inbox", "--all"]);
    assert_eq!(inbox.code, 0, "{}", inbox.stdout);
    // Branch mode: the target is the tip, not a release…
    assert!(inbox.json["data"]["release"].is_null(), "{}", inbox.stdout);
    assert_eq!(inbox.json["data"]["target"], later.as_str());
    assert_eq!(inbox.json["data"]["unreleased"], 0);
    // …but each commit still says which release first shipped it.
    assert_eq!(item(&inbox.json, &shipped)["release"], "v1.0.0");
    assert!(
        item(&inbox.json, &later)
            .get("release")
            .is_none_or(Value::is_null)
    );
    assert_eq!(item(&inbox.json, &later)["state"], "pending");
    assert_eq!(fx.fork.git(&["rev-parse", "v1.0.0"]), fork_tag);
}

#[test]
fn prereleases_count_when_allowed() {
    let w = world("prereleases = true\n");
    let inbox = tether(&w.fork.path, &["inbox"]);
    assert_eq!(
        inbox.json["data"]["release"], "v1.2.0-rc.1",
        "{}",
        inbox.stdout
    );
    assert_eq!(inbox.json["data"]["target"], w.rc.as_str());
    assert_eq!(inbox.json["data"]["unreleased"], 1);
}
