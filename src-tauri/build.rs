fn main() {
    println!("cargo:rerun-if-env-changed=CLASH_BUILD_COMMIT");
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
    };
    // A worktree's HEAD lives outside its checkout. Watch its branch ref too.
    if let Some(path) = git(&["rev-parse", "--git-path", "HEAD"]) {
        println!("cargo:rerun-if-changed={path}");
    }
    if let Some(reference) = git(&["symbolic-ref", "-q", "HEAD"]) {
        if let Some(path) = git(&["rev-parse", "--git-path", &reference]) {
            println!("cargo:rerun-if-changed={path}");
        }
    }
    let commit = std::env::var("CLASH_BUILD_COMMIT")
        .ok()
        .or_else(|| git(&["rev-parse", "HEAD"]))
        .unwrap_or_else(|| "source-archive".into());
    let dirty = git(&["status", "--porcelain=v1"])
        .map(|status| !status.is_empty())
        // Unknown Git state must never identify an artifact as a clean build.
        .unwrap_or(true);
    println!("cargo:rustc-env=CLASH_BUILD_COMMIT={commit}");
    println!("cargo:rustc-env=CLASH_BUILD_DIRTY={dirty}");
    tauri_build::build()
}
