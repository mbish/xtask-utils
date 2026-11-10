use std::path::PathBuf;

use crate::{LinuxDistro, require_package};
use anyhow::Result;
use xshell::{Shell, cmd};

#[derive(serde::Deserialize)]
struct AptlyRepo {
    #[serde(rename = "Name")]
    name: String,
}

#[derive(serde::Deserialize)]
struct AptlyPublications {
    #[serde(rename = "Storage")]
    storage: String,
    #[serde(rename = "Prefix")]
    prefix: String,
}

fn get_aptly_repos(sh: &Shell) -> Result<Vec<AptlyRepo>> {
    let output = cmd!(sh, "aptly repo list -json").read()?;
    Ok(serde_json::from_str(&output)?)
}

fn get_aptly_publications(sh: &Shell) -> Result<Vec<AptlyPublications>> {
    let output = cmd!(sh, "aptly publish list -json").read()?;
    Ok(serde_json::from_str(&output)?)
}

fn require_gpg_key(key_id: &str) -> Result<()> {
    let sh = Shell::new()?;
    let output = cmd!(sh, "gpg --list-keys {key_id}").read()?;
    if !output.contains(key_id) {
        anyhow::bail!("Signing GPG key '{key_id}' is not present. Please import it");
    }
    Ok(())
}

pub struct AptlyPublisher {
    prefix: String,
    s3_config: AptlyS3Config,
    signing_key: String,
    profile: String,
}

#[derive(serde::Deserialize)]
pub struct AptlyS3Config {
    pub bucket: String,
    pub endpoint: String,
}

impl AptlyS3Config {
    pub fn new(bucket: &str, endpoint: &str) -> Self {
        AptlyS3Config {
            bucket: bucket.to_string(),
            endpoint: endpoint.to_string(),
        }
    }
}

#[derive(serde::Deserialize)]
struct AptlyConfig {
    #[serde(rename = "S3PublishEndpoints")]
    s3_publish_endpoints: Option<std::collections::HashMap<String, AptlyS3Config>>,
}

impl AptlyPublisher {
    pub fn new(prefix: &str, s3_config: AptlyS3Config, key_id: String) -> Result<Self> {
        let profile = Self::find_aptly_s3_profile(&s3_config)?;
        let publisher = AptlyPublisher {
            prefix: prefix.to_string(),
            s3_config,
            signing_key: key_id,
            profile,
        };
        Ok(publisher)
    }

    fn find_aptly_s3_profile(s3_config: &AptlyS3Config) -> Result<String> {
        let sh = Shell::new()?;
        let output = cmd!(sh, "aptly config show").read()?;
        let config: AptlyConfig = serde_json::from_str(&output)?;
        let bucket = s3_config.bucket.clone();
        let endpoint = s3_config.endpoint.clone();
        if let Some(s3_endpoints) = config.s3_publish_endpoints {
            for (name, s3_endpoint) in s3_endpoints {
                if s3_endpoint.bucket == bucket && s3_endpoint.endpoint == endpoint {
                    return Ok(name);
                }
            }
        }
        anyhow::bail!(
            "Aptly not configured to access package bucket '{bucket}' and endpoint '{endpoint}'. Please modify your ~/.aptly.conf file to include the appropriate bucket"
        );
    }

    fn get_aptly_repo_name(&self, os: &dyn LinuxDistro) -> String {
        let os = os.get_nickname();
        let prefix = &self.prefix;
        format!("{prefix}-{os}",)
    }

    pub fn publish_repos(&self, version: &str, distros: &Vec<Box<dyn LinuxDistro>>) -> Result<()> {
        let sh = Shell::new()?;
        let time = chrono::Utc::now();
        let aptly_profile = &self.profile;
        let publications = get_aptly_publications(&sh)?;
        let storage = format!("s3:{aptly_profile}");
        sh.set_var("SOURCE_DATE_EPOCH", time.timestamp().to_string());
        for distro in distros.into_iter() {
            let distro_repo = self.get_aptly_repo_name(&**distro);
            let distro_nick = distro.get_nickname();
            let snapshot_name = format!("{version}-{distro_nick}-{}", time.format("%Y%m%d%H%M%S"));
            let key = &self.signing_key;
            cmd!(
                sh,
                "aptly snapshot create {snapshot_name} from repo {distro_repo}"
            )
            .run()?;
            if publications
                .iter()
                .find(|p| p.storage == storage && p.prefix == distro_nick)
                .is_some()
            {
            } else {
                cmd!(
                sh,
                "aptly publish snapshot -gpg-key={key} -distribution={distro_nick} {snapshot_name} s3:{aptly_profile}:{distro_nick}/"
            ).run()?;
            }
            cmd!(
                sh,
                "aptly publish switch -gpg-key={key} {distro_nick} s3:{aptly_profile}:{distro_nick}/ {snapshot_name}"
            ).run()?;
        }
        Ok(())
    }

    fn aptly_create_repo_from_mirror(
        &self,
        distro: &dyn LinuxDistro,
        public_url: &str,
    ) -> Result<()> {
        let sh = Shell::new()?;
        let distro_nickname = distro.get_nickname();
        let prefix = &self.prefix;
        let mirror_name = format!("r2-bootstrap-{distro_nickname}");
        let snapshot_name = format!("base-{prefix}-{distro_nickname}");
        let repo_name = format!("{prefix}-{distro_nickname}");

        // run unconditionally to cleanup any cruft
        let _ = cmd!(sh, "aptly snapshot drop {snapshot_name}").run();
        let _ = cmd!(sh, "aptly mirror drop {mirror_name}").run();

        cmd!(sh, "aptly mirror create -keyring='~/.gnupg/pubring.kbx' -keyring='trustedkeys.gpg' {mirror_name} {public_url}/{distro_nickname} {distro_nickname}").run()?;
        cmd!(sh, "aptly mirror update -keyring='~/.gnupg/pubring.kbx' -keyring='trustedkeys.gpg' {mirror_name}").run()?;
        cmd!(
            sh,
            "aptly snapshot create {snapshot_name} from mirror {mirror_name}"
        )
        .run()?;
        cmd!(sh, "aptly repo create -distribution={distro_nickname} -component=main {repo_name} from snapshot {snapshot_name}").run()?;
        cmd!(sh, "aptly snapshot drop {snapshot_name}").run()?;
        cmd!(sh, "aptly mirror drop {mirror_name}").run()?;
        Ok(())
    }

    pub fn add_package(&self, distro: &dyn LinuxDistro, path: PathBuf) -> Result<()> {
        let sh = Shell::new()?;
        let distro_nickname = distro.get_nickname();
        let prefix = &self.prefix;
        let repo_name = format!("{prefix}-{distro_nickname}");
        cmd!(sh, "aptly repo add {repo_name} {path}").run()?;
        Ok(())
    }

    fn aptly_update_repo_from_mirror(
        &self,
        distro: &dyn LinuxDistro,
        public_url: &str,
    ) -> Result<()> {
        let distro_nickname = distro.get_nickname();
        let prefix = &self.prefix;
        let mirror_name = format!("r2-bootstrap-{distro_nickname}");
        let repo_name = format!("{prefix}-{distro_nickname}");
        let snapshot_name = format!("base-{prefix}-{distro_nickname}");
        let sh = Shell::new()?;

        let _ = cmd!(sh, "aptly snapshot drop {snapshot_name}").run();
        let _ = cmd!(sh, "aptly mirror drop {mirror_name}").run();

        cmd!(sh, "aptly mirror create -keyring='~/.gnupg/pubring.kbx' -keyring='trustedkeys.gpg' {mirror_name} {public_url}/{distro_nickname} {distro_nickname}").run()?;
        cmd!(sh, "aptly mirror update -keyring='~/.gnupg/pubring.kbx' -keyring='trustedkeys.gpg' {mirror_name}").run()?;

        cmd!(sh, "aptly repo import {mirror_name} {repo_name} 'Name'").run()?;
        Ok(())
    }

    pub fn bootstrap(
        &self,
        sh: &Shell,
        public_url: String,
        distros: Vec<Box<dyn LinuxDistro>>,
    ) -> Result<()> {
        require_package("aptly")?;
        require_gpg_key(&self.signing_key)?;
        let repos = get_aptly_repos(sh)?;
        for distro in distros {
            if repos
                .iter()
                .find(|repo| repo.name == self.get_aptly_repo_name(&*distro))
                .is_some()
            {
                self.aptly_update_repo_from_mirror(&*distro, &public_url)?;
            } else {
                self.aptly_create_repo_from_mirror(&*distro, &public_url)?;
            }
        }
        Ok(())
    }
}
