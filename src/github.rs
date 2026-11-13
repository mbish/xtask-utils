use std::{ffi::OsString, path::PathBuf};
use strum::IntoEnumIterator;
use yaml_rust2::YamlLoader;

use anyhow::Result;
use xshell::{Shell, cmd};

use crate::{PackageFormat, Platforms, workspace_root};

#[derive(serde::Deserialize)]
pub struct GithubRepo {
    name: String,
}

impl GithubRepo {
    pub fn new(name: &str) -> Result<Self> {
        Self::require_gh_repo(&name)
    }

    fn require_gh_repo(name: &str) -> Result<GithubRepo> {
        let sh = Shell::new()?;
        cmd!(sh, "gh auth status").ignore_stdout().run()?;
        let gh_repo = {
            let output = cmd!(sh, "gh repo view --json name").read()?;
            serde_json::from_str::<GithubRepo>(&output)?
        };
        if gh_repo.name == name {
            Ok(gh_repo)
        } else {
            anyhow::bail!(
                "Required GitHub repository '{name}' does not exist or is not accessible"
            );
        }
    }

    fn release_exists(&self, version: &str) -> Result<()> {
        let sh = Shell::new()?;
        cmd!(sh, "gh release view {version}")
            .ignore_stdout()
            .run()?;
        Ok(())
    }

    pub fn publish(&self, packages: Vec<String>, version: &str) -> Result<()> {
        let release_notes = get_release_notes(version)?;
        let artifacts = get_build_artifacts(packages, version)?;
        self.publish_artifacts_and_notes(artifacts, release_notes)
    }

    fn publish_artifacts_and_notes(
        &self,
        artifacts: Vec<BuildArtifact>,
        release_notes: ReleaseNotes,
    ) -> Result<()> {
        let sh = Shell::new()?;
        let version = release_notes.get_version();
        let mut to_upload = artifacts
            .iter()
            .filter(|package| package.format != PackageFormat::Deb)
            .cloned()
            .collect::<Vec<_>>();
        for artifact in artifacts {
            if artifact.format == PackageFormat::Deb {
                to_upload.push(alias_release_artifact(&artifact)?);
            }
        }
        let published_paths = to_upload
            .iter()
            .map(|package| package.path.clone())
            .collect::<Vec<_>>();
        if self.release_exists(version).is_err() {
            let notes = get_release_notes(version)?;
            let create_cmd = {
                if release_notes.prerelease {
                    cmd!(
                        sh,
                        "gh release create --prerelease {version} --title {version} -F -"
                    )
                } else {
                    cmd!(sh, "gh release create {version} --title {version} -F -")
                }
            };
            create_cmd.stdin(notes.changes_as_markdown()).run()?;
        }
        if !published_paths.is_empty() {
            let upload_cmd = cmd!(
                sh,
                "gh release upload {version} {published_paths...} --clobber"
            );
            upload_cmd.run()?;
        }
        Ok(())
    }
}
struct ReleaseNotes {
    version: String,
    changes: Vec<String>,
    prerelease: bool,
}

impl ReleaseNotes {
    pub fn get_version(&self) -> &str {
        &self.version
    }
    pub fn changes_as_markdown(&self) -> String {
        self.changes
            .iter()
            .map(|change| format!("- {change}"))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn get_release_notes(version: &str) -> Result<ReleaseNotes> {
    let changelog = workspace_root().join("CHANGES.yaml");
    let docs = YamlLoader::load_from_str(&std::fs::read_to_string(changelog)?)?;
    let first_doc = docs
        .first()
        .ok_or_else(|| anyhow::anyhow!("CHANGES.yaml is empty"))?;
    let changes = first_doc["releases"][version]["changes"]
        .as_vec()
        .ok_or_else(|| anyhow::anyhow!("No changes found for version {version}"))?
        .iter()
        .map(|change| match change.as_str() {
            Some(change_str) => Ok(change_str.to_string()),
            None => anyhow::bail!("Invalid change entry for version {version}"),
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(ReleaseNotes {
        version: version.to_string(),
        changes,
        prerelease: first_doc["releases"][version]["prerelease"]
            .as_bool()
            .unwrap_or(false),
    })
}

#[derive(Clone, Debug)]
struct BuildArtifact {
    pub path: PathBuf,
    pub parent: PathBuf,
    pub package: String,
    pub platform: Platforms,
    pub format: PackageFormat,
    pub os: String,
}

impl BuildArtifact {
    fn alias(&self, new_name: OsString) -> Result<BuildArtifact> {
        let new_path = self.parent.join(new_name);
        std::fs::copy(&self.path, &new_path)?;
        Ok(BuildArtifact {
            os: self.os.clone(),
            platform: self.platform,
            format: self.format,
            path: new_path,
            parent: self.parent.clone(),
            package: self.package.clone(),
        })
    }
}

fn get_build_artifacts(packages: Vec<String>, version: &str) -> Result<Vec<BuildArtifact>> {
    let mut artifacts = Vec::new();
    let artifacts_dir = workspace_root().join("artifacts").join(version);
    for os in std::fs::read_dir(&artifacts_dir)? {
        let os_entry = os?.path();
        let os_name = {
            let Some(os_name) = os_entry.file_name() else {
                eprintln!("Could not get OS name for entry: {:?}", os_entry);
                continue;
            };
            os_name.to_string_lossy().to_string()
        };

        if !os_entry.is_dir() {
            continue;
        }
        for platform in Platforms::iter() {
            let platform_entry = os_entry.join(platform.as_folder());
            if !platform_entry.is_dir() {
                continue;
            }
            for file in std::fs::read_dir(&platform_entry)? {
                let file_entry = file?;
                let path = file_entry.path();
                let Some(package) = packages.iter().find(|p| {
                    file_entry
                        .file_name()
                        .as_encoded_bytes()
                        .starts_with(p.as_bytes())
                }) else {
                    continue;
                };
                if let Some(extension) = path.extension()
                    && let Some(format) = PackageFormat::from_extension(extension)
                {
                    artifacts.push(BuildArtifact {
                        path,
                        parent: platform_entry.clone(),
                        package: package.clone(),
                        platform,
                        format,
                        os: os_name.clone(),
                    })
                }
            }
        }
    }
    Ok(artifacts)
}

fn alias_release_artifact(package: &BuildArtifact) -> Result<BuildArtifact> {
    let os = package.os.to_string();
    let path = &package.path;
    let new_file_name = {
        let current_name = OsString::from(path.file_name().expect("Could not get file name"));
        let mut new_name = OsString::from(format!("{os}-"));
        new_name.push(current_name);
        new_name
    };
    package.alias(new_file_name)
}
