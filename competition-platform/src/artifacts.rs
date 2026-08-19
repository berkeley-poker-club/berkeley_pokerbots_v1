//! Artifact storage on the local filesystem (a shared volume in multi-worker deployments).
//! Layout: `<root>/<submission_id>/artifact.<zip|bin>` and `<root>/<submission_id>/unpacked/`.

use crate::ids::sha256_hex;
use crate::models::Manifest;
use anyhow::{anyhow, bail, Context, Result};
use std::io::Read;
use std::path::{Component, Path, PathBuf};

pub const MAX_UNPACKED_BYTES: u64 = 512 * 1024 * 1024;
pub const MAX_UNPACKED_FILES: usize = 20_000;

pub struct ArtifactStore {
    root: PathBuf,
}

pub struct SavedArtifact {
    pub relative_path: String,
    pub sha256: String,
    pub size: u64,
    pub is_zip: bool,
}

impl ArtifactStore {
    pub fn new(root: &Path) -> Result<Self> {
        std::fs::create_dir_all(root).with_context(|| format!("creating {}", root.display()))?;
        Ok(ArtifactStore {
            root: root.to_path_buf(),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn dir(&self, submission_id: &str) -> PathBuf {
        self.root.join(sanitize(submission_id))
    }

    pub fn unpacked_dir(&self, submission_id: &str) -> PathBuf {
        self.dir(submission_id).join("unpacked")
    }

    /// Persist raw upload bytes. Zip archives are detected by magic number.
    pub fn save(&self, submission_id: &str, bytes: &[u8]) -> Result<SavedArtifact> {
        let is_zip = bytes.starts_with(b"PK\x03\x04");
        let dir = self.dir(submission_id);
        std::fs::create_dir_all(&dir)?;
        let name = if is_zip {
            "artifact.zip"
        } else {
            "artifact.bin"
        };
        let path = dir.join(name);
        std::fs::write(&path, bytes).with_context(|| format!("writing {}", path.display()))?;
        Ok(SavedArtifact {
            relative_path: format!("{}/{}", sanitize(submission_id), name),
            sha256: sha256_hex(bytes),
            size: bytes.len() as u64,
            is_zip,
        })
    }

    /// Unpack (idempotent) and return the directory containing the bot. Single-file uploads are
    /// placed under the manifest entrypoint's file name.
    pub fn unpack(
        &self,
        submission_id: &str,
        relative_path: &str,
        manifest: &Manifest,
    ) -> Result<PathBuf> {
        let dir = self.dir(submission_id);
        let out = dir.join("unpacked");
        let ready = out.join(".pokerbots-ready");
        if ready.exists() {
            return Ok(out);
        }
        let _ = std::fs::remove_dir_all(&out);
        std::fs::create_dir_all(&out)?;
        let artifact = self.root.join(relative_path);
        if relative_path.ends_with(".zip") {
            let file = std::fs::File::open(&artifact)
                .with_context(|| format!("opening {}", artifact.display()))?;
            let mut zip = zip::ZipArchive::new(file).context("reading zip archive")?;
            let mut total: u64 = 0;
            if zip.len() > MAX_UNPACKED_FILES {
                bail!("archive has too many files ({})", zip.len());
            }
            for i in 0..zip.len() {
                let mut entry = zip.by_index(i)?;
                let raw_name = entry.name().to_string();
                let rel = safe_relative(&raw_name)
                    .ok_or_else(|| anyhow!("archive entry '{}' has an unsafe path", raw_name))?;
                if rel.as_os_str().is_empty() {
                    continue;
                }
                let dest = out.join(&rel);
                if entry.is_dir() {
                    std::fs::create_dir_all(&dest)?;
                    continue;
                }
                total += entry.size();
                if total > MAX_UNPACKED_BYTES {
                    bail!("archive expands to more than {} bytes", MAX_UNPACKED_BYTES);
                }
                if let Some(parent) = dest.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                let mut buf = Vec::with_capacity(entry.size() as usize);
                entry.read_to_end(&mut buf)?;
                std::fs::write(&dest, &buf)?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if let Some(mode) = entry.unix_mode() {
                        let _ = std::fs::set_permissions(
                            &dest,
                            std::fs::Permissions::from_mode(mode & 0o777 | 0o600),
                        );
                    }
                }
            }
        } else {
            let ep = manifest.entrypoint.trim_start_matches("./");
            let name = Path::new(ep)
                .file_name()
                .ok_or_else(|| anyhow!("manifest entrypoint has no file name"))?;
            let dest = out.join(name);
            std::fs::copy(&artifact, &dest)
                .with_context(|| format!("copying {}", artifact.display()))?;
        }
        // Common case: a zip made from inside a folder vs. of the folder itself. If the entrypoint
        // is missing at the root but exists exactly one level down, hoist that directory.
        let ep = manifest.entrypoint.trim_start_matches("./");
        if !out.join(ep).exists() {
            let mut candidates = Vec::new();
            for e in std::fs::read_dir(&out)? {
                let e = e?;
                if e.file_type()?.is_dir() && e.path().join(ep).exists() {
                    candidates.push(e.path());
                }
            }
            if candidates.len() == 1 {
                let inner = candidates.remove(0);
                for e in std::fs::read_dir(&inner)? {
                    let e = e?;
                    let target = out.join(e.file_name());
                    if !target.exists() {
                        std::fs::rename(e.path(), target)?;
                    }
                }
                let _ = std::fs::remove_dir_all(&inner);
            }
        }
        let ep_path = out.join(ep);
        if !ep_path.exists() {
            bail!(
                "entrypoint '{}' not found in the uploaded artifact",
                manifest.entrypoint
            );
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(meta) = std::fs::metadata(&ep_path) {
                let mut perms = meta.permissions();
                perms.set_mode(perms.mode() | 0o755);
                let _ = std::fs::set_permissions(&ep_path, perms);
            }
        }
        std::fs::write(&ready, b"ok")?;
        Ok(out)
    }

    pub fn remove(&self, submission_id: &str) -> Result<()> {
        let dir = self.dir(submission_id);
        if dir.exists() {
            std::fs::remove_dir_all(&dir)?;
        }
        Ok(())
    }
}

fn sanitize(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Reject absolute paths and `..` components (zip-slip).
fn safe_relative(name: &str) -> Option<PathBuf> {
    let p = Path::new(name);
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::Normal(n) => out.push(n),
            Component::CurDir => {}
            _ => return None,
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn zip_bytes(entries: &[(&str, &str)]) -> Vec<u8> {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut w = zip::ZipWriter::new(&mut buf);
            let opts = zip::write::SimpleFileOptions::default();
            for (name, content) in entries {
                w.start_file(*name, opts).unwrap();
                w.write_all(content.as_bytes()).unwrap();
            }
            w.finish().unwrap();
        }
        buf.into_inner()
    }

    fn manifest(ep: &str) -> Manifest {
        Manifest {
            language: "python".into(),
            entrypoint: ep.into(),
            runtime: "python3".into(),
            args: vec![],
            build: vec![],
            build_timeout_secs: None,
            notes: None,
            protocol_version: "1".into(),
        }
    }

    #[test]
    fn saves_and_unpacks_zip_and_single_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = ArtifactStore::new(dir.path()).unwrap();
        let z = zip_bytes(&[("bot.py", "print(1)"), ("lib/util.py", "x=1")]);
        let saved = store.save("sub_a_0001", &z).unwrap();
        assert!(saved.is_zip);
        let out = store
            .unpack("sub_a_0001", &saved.relative_path, &manifest("bot.py"))
            .unwrap();
        assert!(out.join("bot.py").exists());
        assert!(out.join("lib/util.py").exists());
        // idempotent
        store
            .unpack("sub_a_0001", &saved.relative_path, &manifest("bot.py"))
            .unwrap();

        let saved = store.save("sub_a_0002", b"print('hi')").unwrap();
        assert!(!saved.is_zip);
        let out = store
            .unpack("sub_a_0002", &saved.relative_path, &manifest("./mybot.py"))
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(out.join("mybot.py")).unwrap(),
            "print('hi')"
        );
    }

    #[test]
    fn hoists_single_top_level_folder() {
        let dir = tempfile::tempdir().unwrap();
        let store = ArtifactStore::new(dir.path()).unwrap();
        let z = zip_bytes(&[("mybot/bot.py", "print(1)"), ("mybot/README", "x")]);
        let saved = store.save("sub_b_0001", &z).unwrap();
        let out = store
            .unpack("sub_b_0001", &saved.relative_path, &manifest("bot.py"))
            .unwrap();
        assert!(out.join("bot.py").exists());
        assert!(out.join("README").exists());
    }

    #[test]
    fn rejects_zip_slip_and_missing_entrypoint() {
        let dir = tempfile::tempdir().unwrap();
        let store = ArtifactStore::new(dir.path()).unwrap();
        let z = zip_bytes(&[("../evil.py", "x")]);
        let saved = store.save("sub_c_0001", &z).unwrap();
        assert!(store
            .unpack("sub_c_0001", &saved.relative_path, &manifest("bot.py"))
            .is_err());
        let z = zip_bytes(&[("other.py", "x")]);
        let saved = store.save("sub_c_0002", &z).unwrap();
        let err = store
            .unpack("sub_c_0002", &saved.relative_path, &manifest("bot.py"))
            .unwrap_err();
        assert!(err.to_string().contains("entrypoint"));
    }
}
