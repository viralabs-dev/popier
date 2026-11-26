//! `popier --update`: replaces the running binary with the one from the latest
//! GitHub release, after checking its SHA-256, like `install.sh` does.

use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use sha2::{Digest, Sha256};

const REPO: &str = "viralabs-dev/popier";
const CURRENT: &str = env!("CARGO_PKG_VERSION");

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

/// Release target of this build, or `None` where no prebuilt binary exists.
fn target() -> Option<&'static str> {
    (cfg!(target_os = "linux") && cfg!(target_arch = "x86_64"))
        .then_some("x86_64-unknown-linux-musl")
}

pub async fn run() -> Result<()> {
    let Some(target) = target() else {
        bail!(
            "there is no prebuilt popier for this system; update it with:\n    \
             cargo install --git https://github.com/{REPO} --force"
        );
    };
    let http = reqwest::Client::builder()
        .user_agent(concat!("popier/", env!("CARGO_PKG_VERSION")))
        .build()?;

    let release: Release = http
        .get(format!(
            "https://api.github.com/repos/{REPO}/releases/latest"
        ))
        .header("Accept", "application/vnd.github+json")
        .send()
        .await?
        .error_for_status()
        .context("cannot read the latest popier release from GitHub")?
        .json()
        .await?;
    let latest = release.tag_name.trim_start_matches('v');
    if !is_newer(latest, CURRENT) {
        println!(
            "popier {CURRENT} is up to date (latest release: {})",
            release.tag_name
        );
        return Ok(());
    }

    let name = format!("popier-{target}.tar.gz");
    let url_of = |name: &str| {
        release
            .assets
            .iter()
            .find(|a| a.name == name)
            .map(|a| a.browser_download_url.clone())
    };
    let archive_url =
        url_of(&name).with_context(|| format!("release {} has no {name}", release.tag_name))?;
    let sum_url = url_of(&format!("{name}.sha256"))
        .with_context(|| format!("release {} has no checksum for {name}", release.tag_name))?;

    println!("downloading popier {} ({name})", release.tag_name);
    let download = |url: String| {
        let http = http.clone();
        async move {
            let bytes = http
                .get(&url)
                .send()
                .await?
                .error_for_status()?
                .bytes()
                .await?;
            anyhow::Ok(bytes)
        }
    };
    let archive = download(archive_url).await.context("download failed")?;
    let sums = download(sum_url)
        .await
        .context("checksum download failed")?;
    let expected = expected_sum(&String::from_utf8_lossy(&sums), &name)?;
    let actual = hex(&Sha256::digest(&archive));
    if actual != expected {
        bail!(
            "checksum mismatch for {name} (expected {expected}, got {actual}); nothing was changed"
        );
    }
    println!("checksum ok");

    let binary = extract(&archive).context("cannot unpack the release archive")?;
    let exe = std::env::current_exe()
        .and_then(|p| p.canonicalize())
        .context("cannot find the running popier binary")?;
    replace(&exe, &binary)?;
    println!("updated {} from {CURRENT} to {latest}", exe.display());
    Ok(())
}

/// Whether version `a` (`1.2.3`) is newer than `b`.
fn is_newer(a: &str, b: &str) -> bool {
    let parts = |v: &str| -> Vec<u64> {
        v.split(['.', '-'])
            .take(3)
            .map(|p| p.parse().unwrap_or(0))
            .collect()
    };
    parts(a) > parts(b)
}

/// The hex digest for `name` in a `sha256sum` line (`<hex>  <name>`), or the
/// only digest in the file.
fn expected_sum(text: &str, name: &str) -> Result<String> {
    let mut lines = text.lines().filter_map(|l| {
        let mut fields = l.split_whitespace();
        Some((
            fields.next()?,
            fields.next().map(|f| f.trim_start_matches('*')),
        ))
    });
    let all: Vec<_> = lines.by_ref().collect();
    let digest = all
        .iter()
        .find(|(_, file)| *file == Some(name))
        .or_else(|| (all.len() == 1).then(|| &all[0]))
        .map(|(digest, _)| digest.to_lowercase())
        .context("the checksum file does not list the archive")?;
    if digest.len() != 64 || !digest.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("the checksum file is malformed");
    }
    Ok(digest)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The `popier` file inside a `.tar.gz`.
fn extract(archive: &[u8]) -> Result<Vec<u8>> {
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(archive));
    for entry in tar.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        if path.file_name().is_some_and(|n| n == "popier") && entry.header().entry_type().is_file()
        {
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes)?;
            return Ok(bytes);
        }
    }
    bail!("the archive has no popier binary")
}

/// Writes `binary` next to `exe` and renames it over `exe`, so a running
/// popier keeps working and a failure leaves the old binary in place.
fn replace(exe: &Path, binary: &[u8]) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let dir = exe.parent().context("the binary has no parent directory")?;
    let tmp: PathBuf = dir.join(".popier.update");
    let write = || -> std::io::Result<()> {
        std::fs::write(&tmp, binary)?;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
        std::fs::rename(&tmp, exe)
    };
    write().map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        if e.kind() == std::io::ErrorKind::PermissionDenied {
            anyhow::anyhow!(
                "no permission to replace {} ({e}); run it with the rights to write there, \
                 or reinstall with the install script",
                exe.display()
            )
        } else {
            anyhow::Error::new(e).context(format!("cannot replace {}", exe.display()))
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions() {
        assert!(is_newer("0.2.0", "0.1.9"));
        assert!(is_newer("0.10.0", "0.9.0"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("0.1.0", "0.2.0"));
    }

    #[test]
    fn reads_sha256sum_files() {
        let d = "a".repeat(64);
        assert_eq!(
            expected_sum(&format!("{d}  popier.tar.gz\n"), "popier.tar.gz").unwrap(),
            d
        );
        assert_eq!(
            expected_sum(&format!("{d} *popier.tar.gz"), "popier.tar.gz").unwrap(),
            d
        );
        assert_eq!(expected_sum(&d, "popier.tar.gz").unwrap(), d);
        assert!(expected_sum("xyz  popier.tar.gz", "popier.tar.gz").is_err());
        assert!(expected_sum("", "popier.tar.gz").is_err());
    }

    #[test]
    fn extracts_and_replaces() {
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        let body = b"#!/bin/sh\necho new\n";
        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        builder
            .append_data(&mut header, "popier", &body[..])
            .unwrap();
        let archive = builder.into_inner().unwrap().finish().unwrap();
        let binary = extract(&archive).unwrap();
        assert_eq!(binary, body);

        let dir = std::env::temp_dir().join(format!("popier-update-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("popier");
        std::fs::write(&exe, b"old").unwrap();
        replace(&exe, &binary).unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), body);
        assert!(!dir.join(".popier.update").exists());
        std::fs::remove_dir_all(&dir).unwrap();

        assert_eq!(hex(&Sha256::digest(b"abc"))[..8], *"ba7816bf");
    }
}
