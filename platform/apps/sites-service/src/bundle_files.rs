//! Filesystem publication uses a complete marker, fsync, and atomic directory rename.
//! This module never constructs a path under a site's data/ directory.
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

use crate::{
    Error, Result,
    bundles::{Descriptor, MAX_FILE_BYTES, digest, validate_path},
    storage::{directory, existing_directory, sync_directory},
};

const MARKER: &str = ".bundle.json";

fn site_root(root: &Path, d: &Descriptor) -> Result<PathBuf> {
    existing_directory(root)?;
    existing_directory(&root.join("sites"))?;
    let site = root.join("sites").join(d.site_id.to_string());
    existing_directory(&site)?;
    Ok(site)
}

fn paths(root: &Path, d: &Descriptor) -> Result<(PathBuf, PathBuf)> {
    let site = site_root(root, d)?;
    let final_path = site.join(d.kind.directory()).join(d.id.to_string());
    let stage = root
        .join("staging")
        .join(format!("{}-{}-{}", d.site_id, d.kind.name(), d.id));
    Ok((stage, final_path))
}

fn present(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

pub(crate) fn publish(
    root: &Path,
    d: &Descriptor,
    files: &BTreeMap<String, Vec<u8>>,
) -> Result<()> {
    let (stage, final_path) = paths(root, d)?;
    directory(&root.join("staging"))?;
    directory(final_path.parent().ok_or(Error::Storage)?)?;
    if present(&final_path)? {
        return verify_final(root, d);
    }
    if present(&stage)? {
        existing_directory(&stage)?;
        // An identical upload may replace only its own incomplete staging area.
        // remove_dir_all does not follow child symlinks. The volume is private.
        fs::remove_dir_all(&stage)?;
    }
    directory(&stage)?;
    for (path, bytes) in files {
        validate_path(path)?;
        let mut parent = stage.clone();
        let parts: Vec<_> = path.split('/').collect();
        for part in &parts[..parts.len() - 1] {
            parent.push(part);
            directory(&parent)?;
        }
        write_new(&parent.join(parts.last().ok_or(Error::Storage)?), bytes)?;
        sync_directory(&parent)?;
    }
    write_new(
        &stage.join(MARKER),
        &serde_json::to_vec(d).map_err(|_| Error::Storage)?,
    )?;
    sync_directory(&stage)?;
    verify_at(&stage, d)?;
    rename_complete(&stage, &final_path)
}

pub(crate) fn recover(root: &Path, d: &Descriptor) -> Result<()> {
    let (stage, final_path) = paths(root, d)?;
    if present(&final_path)? {
        return verify_final(root, d);
    }
    existing_directory(&root.join("staging"))?;
    verify_at(&stage, d)?;
    directory(final_path.parent().ok_or(Error::Storage)?)?;
    rename_complete(&stage, &final_path)
}

fn rename_complete(stage: &Path, final_path: &Path) -> Result<()> {
    // Caller holds the single service publication lock. Final paths are immutable.
    if present(final_path)? {
        return Err(Error::Storage);
    }
    fs::rename(stage, final_path)?;
    sync_directory(final_path.parent().ok_or(Error::Storage)?)?;
    sync_directory(stage.parent().ok_or(Error::Storage)?)?;
    Ok(())
}

pub(crate) fn verify_final(root: &Path, d: &Descriptor) -> Result<()> {
    let (_, final_path) = paths(root, d)?;
    existing_directory(final_path.parent().ok_or(Error::Storage)?)?;
    verify_at(&final_path, d)
}

fn verify_at(base: &Path, d: &Descriptor) -> Result<()> {
    existing_directory(base)?;
    let stored: Descriptor = serde_json::from_slice(&bounded_read(&base.join(MARKER), 256 * 1024)?)
        .map_err(|_| Error::Storage)?;
    if stored != *d {
        return Err(Error::Storage);
    }
    let mut actual = BTreeSet::new();
    walk(base, "", &mut actual, d.files.len() + 1)?;
    let expected: BTreeSet<_> = d
        .files
        .keys()
        .cloned()
        .chain(std::iter::once(MARKER.into()))
        .collect();
    if actual != expected {
        return Err(Error::Storage);
    }
    for path in d.files.keys() {
        checked_file(base, d, path)?;
    }
    Ok(())
}

fn walk(base: &Path, prefix: &str, files: &mut BTreeSet<String>, limit: usize) -> Result<()> {
    for entry in fs::read_dir(base)? {
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| Error::Storage)?;
        let path = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        if path.len() > 512 {
            return Err(Error::Storage);
        }
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            return Err(Error::Storage);
        }
        if kind.is_dir() {
            walk(&entry.path(), &path, files, limit)?;
        } else if kind.is_file() {
            files.insert(path);
            if files.len() > limit {
                return Err(Error::Storage);
            }
        } else {
            return Err(Error::Storage);
        }
    }
    Ok(())
}

pub(crate) fn read_file(root: &Path, d: &Descriptor, path: &str) -> Result<Vec<u8>> {
    let (_, base) = paths(root, d)?;
    existing_directory(base.parent().ok_or(Error::Storage)?)?;
    existing_directory(&base)?;
    checked_file(&base, d, path)
}

fn checked_file(base: &Path, d: &Descriptor, path: &str) -> Result<Vec<u8>> {
    validate_path(path)?;
    let info = d.files.get(path).ok_or(Error::ArtifactNotFound)?;
    let mut parent = base.to_path_buf();
    let parts: Vec<_> = path.split('/').collect();
    for part in &parts[..parts.len() - 1] {
        parent.push(part);
        existing_directory(&parent)?;
    }
    let bytes = bounded_read(&base.join(path), MAX_FILE_BYTES)?;
    if bytes.len() != info.size || digest(&bytes) != info.sha256 {
        return Err(Error::Storage);
    }
    Ok(bytes)
}

fn bounded_read(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > limit as u64 {
        return Err(Error::Storage);
    }
    let mut bytes = Vec::new();
    File::open(path)?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(Error::Storage);
    }
    Ok(bytes)
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}
