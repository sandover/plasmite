use std::path::PathBuf;
use std::process::{Command, Output};

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    target: PathBuf,
    git_config: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("crate");
        std::fs::create_dir_all(root.join("src")).expect("crate directory");
        let git_config = temp.path().join("gitconfig");
        std::fs::write(&git_config, "").expect("isolated Git config");
        std::fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"version-fixture\"\nversion = \"0.8.0\"\nedition = \"2024\"\nbuild = \"build.rs\"\n",
        )
        .expect("Cargo.toml");
        std::fs::copy(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("build_version.rs"),
            root.join("build_version.rs"),
        )
        .expect("copy helper");
        std::fs::write(
            root.join("build.rs"),
            r#"#[path = "build_version.rs"] mod build_version;
fn main() {
    let dir = std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let version = std::env::var("CARGO_PKG_VERSION").unwrap();
    println!("cargo:rustc-env=PLASMITE_BUILD_VERSION={}", build_version::build_identity(&dir, &version));
}
"#,
        )
        .expect("build.rs");
        std::fs::write(
            root.join("src/main.rs"),
            "fn main() { println!(\"{}\", env!(\"PLASMITE_BUILD_VERSION\")); }\n",
        )
        .expect("main.rs");

        let target = temp.path().join("target");
        let fixture = Self {
            _temp: temp,
            root,
            target,
            git_config,
        };
        fixture.git(&["init", "-q"]);
        fixture.git(&["config", "user.name", "Version Test"]);
        fixture.git(&["config", "user.email", "version-test@example.com"]);
        fixture.git(&["add", "."]);
        fixture.git(&["commit", "-qm", "Initial fixture"]);
        fixture
    }

    fn git(&self, args: &[&str]) -> String {
        let output = Command::new("git")
            .current_dir(&self.root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", &self.git_config)
            .args(args)
            .output()
            .expect("run git");
        assert_success(&output, "git", args);
        String::from_utf8(output.stdout)
            .expect("Git output is UTF-8")
            .trim()
            .to_owned()
    }

    fn build(&self) -> Output {
        let output = Command::new("cargo")
            .current_dir(&self.root)
            .env("CARGO_TARGET_DIR", &self.target)
            .env("CARGO_NET_OFFLINE", "true")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", &self.git_config)
            .args(["build", "--offline", "-vv"])
            .output()
            .expect("run cargo build");
        assert_success(&output, "cargo build", &[]);
        output
    }

    fn identity(&self) -> String {
        let binary = self
            .target
            .join("debug")
            .join(format!("version-fixture{}", std::env::consts::EXE_SUFFIX));
        let output = Command::new(binary).output().expect("run fixture");
        assert_success(&output, "fixture", &[]);
        String::from_utf8(output.stdout)
            .expect("fixture output is UTF-8")
            .trim()
            .to_owned()
    }
}

fn assert_success(output: &Output, command: &str, args: &[&str]) {
    assert!(
        output.status.success(),
        "{command} {args:?} failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn log(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn cargo_rebuilds_when_build_identity_inputs_change() {
    let fixture = Fixture::new();
    let first_log = log(&fixture.build());
    assert!(first_log.contains("Running"), "first build:\n{first_log}");
    let first_hash = fixture.git(&["rev-parse", "--short=7", "HEAD"]);
    let first = format!("0.8.0-dev+g{first_hash}");
    assert_eq!(fixture.identity(), first);

    let fresh_log = log(&fixture.build());
    assert!(
        fresh_log.contains("Fresh version-fixture"),
        "second build:\n{fresh_log}"
    );
    assert!(
        !fresh_log.contains("build-script-build"),
        "build script ran:\n{fresh_log}"
    );

    std::fs::write(
        fixture.root.join("src/main.rs"),
        "// tracked source edit\nfn main() { println!(\"{}\", env!(\"PLASMITE_BUILD_VERSION\")); }\n",
    )
    .expect("edit source");
    let edit_log = log(&fixture.build());
    assert!(
        edit_log.contains("Dirty version-fixture") && edit_log.contains("Running"),
        "tracked edit:\n{edit_log}"
    );
    assert_eq!(fixture.identity(), format!("{first}.dirty"));

    fixture.git(&["add", "src/main.rs"]);
    fixture.git(&["commit", "-qm", "Edit fixture source"]);
    let commit_log = log(&fixture.build());
    assert!(
        commit_log.contains("Dirty version-fixture") && commit_log.contains("Running"),
        "commit:\n{commit_log}"
    );
    let committed = format!(
        "0.8.0-dev+g{}",
        fixture.git(&["rev-parse", "--short=7", "HEAD"])
    );
    assert_ne!(committed, first);
    assert_eq!(fixture.identity(), committed);

    fixture.git(&["tag", "v0.8.0"]);
    let tag_log = log(&fixture.build());
    assert!(
        tag_log.contains("Dirty version-fixture") && tag_log.contains("Running"),
        "tag:\n{tag_log}"
    );
    assert_eq!(fixture.identity(), "0.8.0");

    fixture.git(&["tag", "-d", "v0.8.0"]);
    let remove_tag_log = log(&fixture.build());
    assert!(
        remove_tag_log.contains("Dirty version-fixture") && remove_tag_log.contains("Running"),
        "tag removal:\n{remove_tag_log}"
    );
    assert_eq!(fixture.identity(), committed);
}
