use crate::runtime;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Component, Path, PathBuf},
};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GithubSource {
    pub repository: String,
    pub reference: Option<String>,
    #[serde(default = "default_context")]
    pub context: String,
}

fn default_context() -> String {
    ".".into()
}

impl GithubSource {
    pub fn validate(&self) -> Result<()> {
        let repository = self
            .repository
            .strip_prefix("https://github.com/")
            .context("Use a public https://github.com/owner/repository URL")?;
        let repository = repository.strip_suffix('/').unwrap_or(repository);
        let repository = repository.strip_suffix(".git").unwrap_or(repository);
        let parts: Vec<_> = repository.split('/').collect();
        ensure!(
            parts.len() == 2
                && parts.iter().all(|part| {
                    !part.is_empty()
                        && part.len() <= 100
                        && part.as_bytes()[0].is_ascii_alphanumeric()
                        && part
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
                }),
            "Use a public https://github.com/owner/repository URL without credentials, query or fragment"
        );
        if let Some(reference) = &self.reference {
            ensure!(
                !reference.is_empty()
                    && reference.len() <= 256
                    && !reference.starts_with(['-', '/'])
                    && !reference.ends_with(['.', '/'])
                    && !reference.contains("..")
                    && !reference.contains("//")
                    && reference
                        .split('/')
                        .all(|part| !part.starts_with('.') && !part.ends_with(".lock"))
                    && reference
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"/_-.".contains(&b)),
                "GitHub reference must be a branch or tag name using letters, numbers, /, _, - or ."
            );
        }
        ensure!(
            !self.context.is_empty()
                && self.context.len() <= 1024
                && !self.context.contains(['\\', ':'])
                && !self.context.contains(char::is_control)
                && Path::new(&self.context)
                    .components()
                    .all(|part| matches!(part, Component::Normal(_) | Component::CurDir)),
            "GitHub build context must be a relative directory inside the repository"
        );
        Ok(())
    }

    pub(crate) async fn checkout(&self, workspace: &Path, git: &[String]) -> Result<Checkout> {
        self.validate()?;
        let directory = workspace.join(format!(".github-build-{:032x}", rand::random::<u128>()));
        fs::create_dir(&directory).context("Could not create GitHub build directory")?;
        let checkout = Checkout(directory);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&checkout.0, fs::Permissions::from_mode(0o700))?;
        }
        let mut command = git.to_vec();
        command.extend(
            [
                "-c",
                "credential.helper=",
                "-c",
                "credential.interactive=false",
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "http.followRedirects=false",
                "-c",
                "protocol.file.allow=never",
                "-c",
                "protocol.ext.allow=never",
                "clone",
                "--template=",
                "--depth",
                "1",
                "--single-branch",
                "--no-tags",
            ]
            .map(String::from),
        );
        if let Some(reference) = &self.reference {
            command.extend(["--branch".into(), reference.clone()]);
        }
        command.extend([
            "--".into(),
            self.repository.trim_end_matches('/').into(),
            checkout
                .0
                .to_str()
                .context("Build path must be UTF-8")?
                .into(),
        ]);
        runtime::run_git(&command).await.context("Could not clone public GitHub repository; check the URL and branch/tag. Private repositories require a local checkout")?;
        Ok(checkout)
    }
}

pub(crate) struct Checkout(PathBuf);

impl Checkout {
    pub(crate) async fn archive(&self, relative: &str) -> Result<PathBuf> {
        let context = self.context(relative)?;
        let archive = self.0.with_extension("tar");
        runtime::archive_context(&context, &archive).await?;
        Ok(archive)
    }

    pub(crate) fn context(&self, relative: &str) -> Result<PathBuf> {
        let mut context = self.0.clone();
        for component in Path::new(relative).components() {
            match component {
                Component::CurDir => (),
                Component::Normal(part) => {
                    context.push(part);
                    let metadata = fs::symlink_metadata(&context)
                        .context("GitHub build context is unavailable")?;
                    ensure!(
                        metadata.is_dir() && !metadata.is_symlink(),
                        "GitHub build context must be a directory without symlinks"
                    );
                }
                _ => anyhow::bail!("GitHub build context must stay inside the repository"),
            }
        }
        let dockerfile = fs::symlink_metadata(context.join("Dockerfile"))
            .context("GitHub build context must contain a Dockerfile")?;
        ensure!(
            dockerfile.is_file() && !dockerfile.is_symlink(),
            "Dockerfile must be a regular file inside the repository"
        );
        Ok(context)
    }
}

impl Drop for Checkout {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
        let _ = fs::remove_file(self.0.with_extension("tar"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(repository: &str, reference: Option<&str>, context: &str) -> GithubSource {
        GithubSource {
            repository: repository.into(),
            reference: reference.map(String::from),
            context: context.into(),
        }
    }

    #[test]
    fn validates_github_url_reference_and_relative_context() {
        for repository in [
            "https://github.com/example/app",
            "https://github.com/example/app.git/",
        ] {
            assert!(
                source(repository, Some("release/v1.2"), "services/api")
                    .validate()
                    .is_ok()
            );
        }
        for repository in [
            "http://github.com/a/b",
            "https://github.com@evil.test/a/b",
            "https://github.com/a/b?token=x",
            "https://github.com/a/b/tree/main",
            "https://github.com/../b",
            "git@github.com:a/b",
            "file:///tmp/repo",
        ] {
            assert!(
                source(repository, None, ".").validate().is_err(),
                "{repository}"
            );
        }
        for reference in [
            "-main",
            "a..b",
            "foo.lock",
            "main:other",
            "x@{y}",
            "main\n",
            "/main",
            "main//child",
        ] {
            assert!(
                source("https://github.com/a/b", Some(reference), ".")
                    .validate()
                    .is_err(),
                "{reference}"
            );
        }
        for context in [
            "../outside",
            "/tmp/app",
            "a/../../b",
            "a\\..\\b",
            "C:app",
            "",
        ] {
            assert!(
                source("https://github.com/a/b", None, context)
                    .validate()
                    .is_err(),
                "{context}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_context_or_dockerfile_and_cleans_checkout() {
        use std::os::unix::fs::symlink;
        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let path = workspace.path().join("checkout");
        fs::create_dir(&path).unwrap();
        fs::write(outside.path().join("Dockerfile"), "FROM scratch").unwrap();
        let checkout = Checkout(path.clone());
        symlink(outside.path(), path.join("escape")).unwrap();
        assert!(checkout.context("escape").is_err());
        symlink(outside.path().join("Dockerfile"), path.join("Dockerfile")).unwrap();
        assert!(checkout.context(".").is_err());
        drop(checkout);
        assert!(!path.exists());
        assert!(outside.path().join("Dockerfile").exists());
    }
}
