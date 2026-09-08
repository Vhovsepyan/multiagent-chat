//! Read repository files without traversing repository-supplied links.

use std::fs::{self, File};
use std::path::{Component, Path};

use anyhow::{Result, bail};

pub fn is_link(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        // Includes junctions and other reparse points, not just symlinks.
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

pub fn open(root: &Path, path: &Path) -> Result<File> {
    let relative = path.strip_prefix(root)?;
    let root = root.canonicalize()?;
    let mut checked = root.clone();
    for component in relative.components() {
        let Component::Normal(name) = component else {
            bail!("invalid repository file path");
        };
        checked.push(name);
        if is_link(&fs::symlink_metadata(&checked)?) {
            bail!("repository file links are not supported");
        }
    }
    if !checked.canonicalize()?.starts_with(&root) {
        bail!("repository file escapes the workspace");
    }
    if !fs::symlink_metadata(&checked)?.is_file() {
        bail!("repository path is not a regular file");
    }
    Ok(File::open(checked)?)
}
