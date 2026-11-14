//! A fresh reviewer reads a separate clone of committed repository state.

use codex_utils_absolute_path::AbsolutePathBuf;
use serde::Deserialize;
use serde::Serialize;
use std::path::Path;
use std::path::PathBuf;
use tokio::process::Command;
use uuid::Uuid;

pub(crate) struct ReviewCheckout {
    pub(crate) root: AbsolutePathBuf,
    pub(crate) cwd: AbsolutePathBuf,
    pub(crate) instructions: String,
}

const REVIEW_METADATA_FILE: &str = "multidex-reviewer.json";

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ReviewTarget {
    repository: Option<String>,
    branch: Option<String>,
    pull_request: Option<u64>,
}

#[derive(Debug, Deserialize, Serialize)]
struct ReviewMetadata {
    version: u8,
    root: PathBuf,
    cwd: PathBuf,
    commit: String,
    target: ReviewTarget,
}

fn review_instructions(target: &ReviewTarget) -> String {
    let mut instructions = String::from(
        "\n\nYou are an independent reviewer. Use only this clean checkout and the review task, not the parent's conversation or working tree. Review the committed changes, then return your findings to the parent with file paths and line numbers. If no issues are found, say so plainly.",
    );
    if let Some(repository) = target.repository.as_deref() {
        if let Some(number) = target.pull_request {
            instructions.push_str(&format!(
                "\nThis review targets https://github.com/{repository}/pull/{number}. Compare its checked-out head with the PR base. After finishing, post the same review as one PR discussion comment with `gh pr comment {number} --repo {repository} --body-file <review-file>`. Check whether a prior comment from this review exists before posting, and include the comment URL in your final response to the parent."
            ));
        } else if let Some(branch) = target.branch.as_deref() {
            instructions.push_str(&format!(
                "\nThis review targets branch `{branch}` in `{repository}`. Compare it with the repository's default branch. Report findings only to the parent; there is no PR discussion to post."
            ));
        }
    }
    instructions
}

async fn checkout_commit(root: &Path) -> Result<String, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--verify", "HEAD"])
        .output()
        .await
        .map_err(|err| format!("could not read review commit: {err}"))?;
    if !output.status.success() {
        return Err("reviewer checkout has no committed HEAD".into());
    }
    String::from_utf8(output.stdout)
        .map(|value| value.trim().to_string())
        .map_err(|err| format!("Git returned an invalid review commit: {err}"))
}

async fn persist_checkout(
    root: AbsolutePathBuf,
    cwd: AbsolutePathBuf,
    target: ReviewTarget,
) -> Result<ReviewCheckout, String> {
    let root = AbsolutePathBuf::from_absolute_path(
        root.as_path()
            .canonicalize()
            .map_err(|err| format!("could not resolve reviewer checkout: {err}"))?,
    )
    .map_err(|err| format!("reviewer checkout path is invalid: {err}"))?;
    let cwd = AbsolutePathBuf::from_absolute_path(
        cwd.as_path()
            .canonicalize()
            .map_err(|err| format!("could not resolve reviewer working directory: {err}"))?,
    )
    .map_err(|err| format!("reviewer working directory is invalid: {err}"))?;
    let commit = checkout_commit(root.as_path()).await?;
    let metadata = ReviewMetadata {
        version: 1,
        root: root.to_path_buf(),
        cwd: cwd.to_path_buf(),
        commit,
        target: target.clone(),
    };
    let bytes = serde_json::to_vec(&metadata)
        .map_err(|err| format!("could not encode reviewer checkout metadata: {err}"))?;
    std::fs::write(root.join(".git").join(REVIEW_METADATA_FILE), bytes)
        .map_err(|err| format!("could not persist reviewer checkout metadata: {err}"))?;
    Ok(ReviewCheckout {
        root,
        cwd,
        instructions: review_instructions(&target),
    })
}

/// A cold wake must use the original checkout. Fail closed if its persisted record or
/// checkout is gone, moved, or changed; never fall back to the parent's workspace.
pub(crate) async fn restore_checkout(
    stored_cwd: &Path,
    codex_home: &AbsolutePathBuf,
) -> Result<ReviewCheckout, String> {
    let base = codex_home.as_path().join("review-clones");
    let base = base
        .canonicalize()
        .map_err(|err| format!("reviewer checkout directory is unavailable: {err}"))?;
    let cwd = stored_cwd
        .canonicalize()
        .map_err(|err| format!("reviewer working directory is unavailable: {err}"))?;
    let relative = cwd
        .strip_prefix(&base)
        .map_err(|_| "reviewer working directory is outside review-clones".to_string())?;
    let Some(std::path::Component::Normal(name)) = relative.components().next() else {
        return Err("reviewer checkout path is invalid".into());
    };
    let root = base.join(name);
    let metadata_path = root.join(".git").join(REVIEW_METADATA_FILE);
    let metadata: ReviewMetadata = serde_json::from_slice(
        &std::fs::read(&metadata_path)
            .map_err(|err| format!("reviewer checkout metadata is unavailable: {err}"))?,
    )
    .map_err(|err| format!("reviewer checkout metadata is invalid: {err}"))?;
    if metadata.version != 1
        || metadata.root != root
        || metadata.cwd != cwd
        || !root.join(".git").is_dir()
        || metadata.commit != checkout_commit(&root).await?
    {
        return Err("reviewer checkout metadata does not match the saved thread".into());
    }
    let root = AbsolutePathBuf::from_absolute_path(root)
        .map_err(|err| format!("reviewer checkout path is invalid: {err}"))?;
    let cwd = AbsolutePathBuf::from_absolute_path(cwd)
        .map_err(|err| format!("reviewer working directory is invalid: {err}"))?;
    Ok(ReviewCheckout {
        root,
        cwd,
        instructions: review_instructions(&metadata.target),
    })
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
        .args(["clone", "--local", "--no-hardlinks", "--no-checkout"])
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
    persist_checkout(
        root,
        cwd,
        ReviewTarget {
            repository: None,
            branch: None,
            pull_request: None,
        },
    )
    .await
}

pub(crate) fn github_repository_slug(value: &str) -> Result<String, String> {
    let trimmed = value.trim().trim_end_matches('/');
    let slug = trimmed
        .strip_prefix("https://github.com/")
        .unwrap_or(trimmed);
    let slug = slug.strip_suffix(".git").unwrap_or(slug);
    let parts = slug.split('/').collect::<Vec<_>>();
    if parts.len() != 2
        || parts.iter().any(|part| {
            part.is_empty()
                || part.starts_with('-')
                || part.starts_with('.')
                || !part
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
        })
    {
        return Err("review_repository must be a GitHub owner/repo or HTTPS repository URL".into());
    }
    Ok(slug.to_string())
}

pub(crate) async fn create_github_checkout(
    codex_home: &AbsolutePathBuf,
    repository: &str,
    branch: Option<&str>,
    pull_request: Option<u64>,
) -> Result<ReviewCheckout, String> {
    let slug = github_repository_slug(repository)?;
    if branch.is_some() && pull_request.is_some() {
        return Err("review_ref and review_pull_request cannot be used together".into());
    }
    if pull_request == Some(0) {
        return Err("review_pull_request must be greater than zero".into());
    }
    if let Some(branch) = branch {
        if branch.starts_with('-')
            || !Command::new("git")
                .args(["check-ref-format", "--branch", branch])
                .output()
                .await
                .map_err(|err| format!("could not validate review branch: {err}"))?
                .status
                .success()
        {
            return Err("review_ref must be a valid Git branch name".into());
        }
    }

    let checkout_parent = codex_home.as_path().join("review-clones");
    std::fs::create_dir_all(&checkout_parent)
        .map_err(|err| format!("could not create reviewer workspace directory: {err}"))?;
    let checkout_path = checkout_parent.join(format!("review-{}", Uuid::new_v4()));
    let url = format!("https://github.com/{slug}.git");
    let output = Command::new("git")
        .args(["clone", "--filter=blob:none", "--no-checkout"])
        .arg(&url)
        .arg(&checkout_path)
        .output()
        .await
        .map_err(|err| format!("could not clone review repository: {err}"))?;
    if !output.status.success() {
        return Err(format!(
            "could not clone review repository: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }

    let revision = if let Some(number) = pull_request {
        let output = Command::new("git")
            .arg("-C")
            .arg(&checkout_path)
            .args(["fetch", "--no-tags", "origin"])
            .arg(format!("refs/pull/{number}/head"))
            .output()
            .await
            .map_err(|err| format!("could not fetch review PR: {err}"))?;
        if !output.status.success() {
            let _ = std::fs::remove_dir_all(&checkout_path);
            return Err(format!(
                "could not fetch review PR: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        "FETCH_HEAD".to_string()
    } else if let Some(branch) = branch {
        format!("refs/remotes/origin/{branch}")
    } else {
        "HEAD".to_string()
    };
    let output = Command::new("git")
        .arg("-C")
        .arg(&checkout_path)
        .args(["checkout", "--detach"])
        .arg(revision)
        .output()
        .await
        .map_err(|err| format!("could not check out review revision: {err}"))?;
    if !output.status.success() {
        let _ = std::fs::remove_dir_all(&checkout_path);
        return Err(format!(
            "could not check out review revision: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let root = AbsolutePathBuf::from_absolute_path(&checkout_path)
        .map_err(|err| format!("reviewer checkout path is invalid: {err}"))?;
    persist_checkout(
        root.clone(),
        root,
        ReviewTarget {
            repository: Some(slug),
            branch: branch.map(str::to_string),
            pull_request,
        },
    )
    .await
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
        let restored = restore_checkout(first.cwd.as_path(), &home).await.unwrap();
        assert_eq!(restored.root, first.root);
        assert_eq!(restored.cwd, first.cwd);
        assert!(restored.instructions.contains("independent reviewer"));
        assert!(restore_checkout(&cwd, &home).await.is_err());
        std::fs::remove_file(first.root.join(".git").join(REVIEW_METADATA_FILE)).unwrap();
        assert!(restore_checkout(first.cwd.as_path(), &home).await.is_err());
    }

    #[tokio::test]
    async fn pr_review_target_survives_checkout_restore() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        let home = AbsolutePathBuf::from_absolute_path(temp.path().join("home")).unwrap();
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        std::fs::write(repo.join("file.txt"), "committed\n").unwrap();
        git(&repo, &["add", "."]);
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
        let cwd = AbsolutePathBuf::from_absolute_path(&repo).unwrap();
        let checkout = create_clean_checkout(&cwd, &home).await.unwrap();
        let checkout = persist_checkout(
            checkout.root,
            checkout.cwd,
            ReviewTarget {
                repository: Some("ModelCloud/GPTQModel".into()),
                branch: None,
                pull_request: Some(3457),
            },
        )
        .await
        .unwrap();
        let restored = restore_checkout(checkout.cwd.as_path(), &home)
            .await
            .unwrap();
        assert!(restored.instructions.contains("pull/3457"));
        assert!(restored.instructions.contains("gh pr comment 3457"));
    }

    #[test]
    fn github_repository_names_are_bounded() {
        assert_eq!(
            github_repository_slug("https://github.com/ModelCloud/GPTQModel.git").unwrap(),
            "ModelCloud/GPTQModel"
        );
        assert_eq!(
            github_repository_slug("ModelCloud/GPTQModel").unwrap(),
            "ModelCloud/GPTQModel"
        );
        assert!(github_repository_slug("https://example.com/owner/repo").is_err());
        assert!(github_repository_slug("owner/../repo").is_err());
    }
}
