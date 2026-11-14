//! A fresh reviewer reads a separate clone of committed repository state.

use codex_utils_absolute_path::AbsolutePathBuf;
use std::path::Path;
use tokio::process::Command;
use uuid::Uuid;

pub(crate) struct ReviewCheckout {
    pub(crate) root: AbsolutePathBuf,
    pub(crate) cwd: AbsolutePathBuf,
}

pub(crate) async fn create_clean_checkout(
    parent_cwd: &AbsolutePathBuf,
    codex_home: &AbsolutePathBuf,
) -> Result<ReviewCheckout, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(parent_cwd.as_path())
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .await
        .map_err(|err| format!("reviewer needs Git to create a clean checkout: {err}"))?;
    if !output.status.success() {
        return Err("reviewer needs a Git repository with a committed HEAD".to_string());
    }
    let repo_root = String::from_utf8(output.stdout)
        .map_err(|err| format!("Git returned an invalid repository path: {err}"))?;
    let repo_root = Path::new(repo_root.trim());
    let canonical_repo = repo_root
        .canonicalize()
        .map_err(|err| format!("could not resolve review repository: {err}"))?;
    let canonical_cwd = parent_cwd
        .as_path()
        .canonicalize()
        .map_err(|err| format!("could not resolve review directory: {err}"))?;
    let relative_cwd = canonical_cwd
        .strip_prefix(&canonical_repo)
        .map_err(|_| "review directory is outside its Git repository".to_string())?;

    let head = Command::new("git")
        .arg("-C")
        .arg(&canonical_repo)
        .args(["rev-parse", "--verify", "HEAD"])
        .output()
        .await
        .map_err(|err| format!("could not read review commit: {err}"))?;
    if !head.status.success() {
        return Err("reviewer needs a Git repository with a committed HEAD".to_string());
    }
    let commit = String::from_utf8(head.stdout)
        .map_err(|err| format!("Git returned an invalid review commit: {err}"))?;

    let checkout_parent = codex_home.as_path().join("review-clones");
    std::fs::create_dir_all(&checkout_parent)
        .map_err(|err| format!("could not create reviewer workspace directory: {err}"))?;
    let checkout_path = checkout_parent.join(format!("review-{}", Uuid::new_v4()));
    let output = Command::new("git")
        .args(["clone", "--shared", "--no-checkout", "--local"])
        .arg(&canonical_repo)
        .arg(&checkout_path)
        .output()
        .await
        .map_err(|err| format!("could not create reviewer checkout: {err}"))?;
    if !output.status.success() {
        return Err(format!(
            "could not create reviewer checkout: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let output = Command::new("git")
        .arg("-C")
        .arg(&checkout_path)
        .args(["checkout", "--detach"])
        .arg(commit.trim())
        .output()
        .await
        .map_err(|err| format!("could not check out review commit: {err}"))?;
    if !output.status.success() {
        let _ = std::fs::remove_dir_all(&checkout_path);
        return Err(format!(
            "could not check out review commit: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let root = AbsolutePathBuf::from_absolute_path(&checkout_path)
        .map_err(|err| format!("reviewer checkout path is invalid: {err}"))?;
    let relative_path = checkout_path.join(relative_cwd);
    let cwd_path = if relative_path.is_dir() {
        relative_path
    } else {
        checkout_path
    };
    let cwd = AbsolutePathBuf::from_absolute_path(&cwd_path)
        .map_err(|err| format!("reviewer working directory is invalid: {err}"))?;
    Ok(ReviewCheckout { root, cwd })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(repo: &Path, args: &[&str]) {
        assert!(
            std::process::Command::new("git")
                .arg("-C")
                .arg(repo)
                .args(args)
                .status()
                .expect("git should run")
                .success()
        );
    }

    #[tokio::test]
    async fn each_reviewer_gets_a_distinct_clean_checkout() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        let home = temp.path().join("home");
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::write(repo.join("src/file.txt"), "committed\n").unwrap();
        git(&repo, &["init", "-q"]);
        git(
            &repo,
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "add",
                ".",
            ],
        );
        git(
            &repo,
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-qm",
                "initial",
            ],
        );
        std::fs::write(repo.join("src/file.txt"), "uncommitted\n").unwrap();
        let cwd = AbsolutePathBuf::from_absolute_path(repo.join("src")).unwrap();
        let home = AbsolutePathBuf::from_absolute_path(home).unwrap();
        let first = create_clean_checkout(&cwd, &home).await.unwrap();
        let second = create_clean_checkout(&cwd, &home).await.unwrap();
        assert_ne!(first.root, second.root);
        assert_ne!(first.cwd, cwd);
        assert!(first.root.join(".git").is_dir());
        assert!(second.root.join(".git").is_dir());
        assert_eq!(
            std::fs::read_to_string(first.cwd.join("file.txt")).unwrap(),
            "committed\n"
        );
        assert_eq!(
            std::fs::read_to_string(second.cwd.join("file.txt")).unwrap(),
            "committed\n"
        );
    }
}
