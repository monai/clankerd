//! Place the complete guest management environment in each virtio-fs boot root.
use crate::{Error, Result};
use std::{fs, path::Path};
pub(crate) fn place(source: &Path, target: &Path) -> Result<()> {
    let management = source.join("management");
    if !management.join("sbin/dhcpcd").is_file() || !management.join("sbin/mke2fs").is_file() {
        return Err(Error::unavailable(format!(
            "guest management environment missing in {}; run make build",
            source.display()
        )));
    }
    copy_tree(&management, target)
}
fn copy_tree(source: &Path, target: &Path) -> Result<()> {
    fs::create_dir_all(target)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let destination = target.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            if fs::symlink_metadata(&destination).is_ok() {
                fs::remove_file(&destination)?;
            }
            std::os::unix::fs::symlink(fs::read_link(entry.path())?, destination)?;
        } else if kind.is_dir() {
            copy_tree(&entry.path(), &destination)?;
        } else if kind.is_file() {
            // Never mutate packaged configuration through hard links.
            if fs::symlink_metadata(&destination).is_ok() {
                fs::remove_file(&destination)?;
            }
            fs::copy(entry.path(), destination)?;
        } else {
            return Err(Error::unavailable(
                "unexpected special file in management environment",
            ));
        }
    }
    fs::set_permissions(target, fs::metadata(source)?.permissions())?;
    Ok(())
}
