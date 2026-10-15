use std::process::Command;

// Since I am currently not updating the version number in Cargo.toml, I will use the git commit hash to track versions.
fn main() {
    let output = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output();

    let commit_hash = match output {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).trim().to_string(),
        _ => "unknown".to_string(),
    };

    println!("cargo:rustc-env=GIT_COMMIT_HASH={}", commit_hash);

    println!("cargo:rerun-if-changed=.git/HEAD");
}
