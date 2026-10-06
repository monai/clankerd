//! Builds synthetic tars for seam B. Expected values in tests are written as
//! literals next to the entries, never recomputed by the code under test.
#![allow(dead_code)]

use tar::{Builder, EntryType, Header};

pub struct Tb {
    b: Builder<Vec<u8>>,
    pub uid: u64,
    pub gid: u64,
}

pub struct Meta<'a> {
    pub mode: u32,
    pub mtime: u64,
    pub uid: Option<u64>,
    pub gid: Option<u64>,
    /// PAX records applied to this entry, e.g. `("SCHILY.xattr.user.x", b"v")`.
    pub pax: Vec<(&'a str, Vec<u8>)>,
}

impl Default for Meta<'_> {
    fn default() -> Self {
        Meta {
            mode: 0o644,
            mtime: 1_700_000_000,
            uid: None,
            gid: None,
            pax: vec![],
        }
    }
}

impl<'a> Meta<'a> {
    pub fn mode(mut self, mode: u32) -> Self {
        self.mode = mode;
        self
    }
    pub fn mtime(mut self, mtime: u64) -> Self {
        self.mtime = mtime;
        self
    }
    pub fn owner(mut self, uid: u64, gid: u64) -> Self {
        self.uid = Some(uid);
        self.gid = Some(gid);
        self
    }
    pub fn pax(mut self, key: &'a str, value: &[u8]) -> Self {
        self.pax.push((key, value.to_vec()));
        self
    }
}

impl Tb {
    /// Entries default to the caller's own uid/gid so unprivileged runs can apply them.
    pub fn new() -> Self {
        // SAFETY: no preconditions.
        let (uid, gid) = unsafe { (libc::geteuid() as u64, libc::getegid() as u64) };
        Tb {
            b: Builder::new(Vec::new()),
            uid,
            gid,
        }
    }

    fn header(&self, ty: EntryType, size: u64, m: &Meta) -> Header {
        let mut h = Header::new_gnu();
        h.set_entry_type(ty);
        h.set_size(size);
        h.set_mode(m.mode);
        h.set_mtime(m.mtime);
        h.set_uid(m.uid.unwrap_or(self.uid));
        h.set_gid(m.gid.unwrap_or(self.gid));
        h
    }

    fn pax(&mut self, m: &Meta) {
        if !m.pax.is_empty() {
            self.b
                .append_pax_extensions(m.pax.iter().map(|(k, v)| (*k, v.as_slice())))
                .unwrap();
        }
    }

    pub fn dir(mut self, path: &str, m: Meta) -> Self {
        self.pax(&m);
        let mut h = self.header(EntryType::Directory, 0, &m);
        self.b.append_data(&mut h, path, std::io::empty()).unwrap();
        self
    }

    pub fn file(mut self, path: &str, data: &[u8], m: Meta) -> Self {
        self.pax(&m);
        let mut h = self.header(EntryType::Regular, data.len() as u64, &m);
        self.b.append_data(&mut h, path, data).unwrap();
        self
    }

    /// A file whose raw name field is written verbatim (the tar crate refuses `..`).
    pub fn file_raw_name(mut self, name: &str, data: &[u8], m: Meta) -> Self {
        let mut h = self.header(EntryType::Regular, data.len() as u64, &m);
        h.as_old_mut().name[..name.len()].copy_from_slice(name.as_bytes());
        h.set_cksum();
        self.b.append(&h, data).unwrap();
        self
    }

    pub fn symlink(mut self, path: &str, target: &str, m: Meta) -> Self {
        self.pax(&m);
        let mut h = self.header(EntryType::Symlink, 0, &m);
        self.b.append_link(&mut h, path, target).unwrap();
        self
    }

    pub fn hardlink(mut self, path: &str, target: &str, m: Meta) -> Self {
        self.pax(&m);
        let mut h = self.header(EntryType::Link, 0, &m);
        self.b.append_link(&mut h, path, target).unwrap();
        self
    }

    pub fn fifo(mut self, path: &str, m: Meta) -> Self {
        self.pax(&m);
        let mut h = self.header(EntryType::Fifo, 0, &m);
        self.b.append_data(&mut h, path, std::io::empty()).unwrap();
        self
    }

    pub fn device(mut self, path: &str, block: bool, major: u32, minor: u32, m: Meta) -> Self {
        self.pax(&m);
        let ty = if block {
            EntryType::Block
        } else {
            EntryType::Char
        };
        let mut h = self.header(ty, 0, &m);
        h.set_device_major(major).unwrap();
        h.set_device_minor(minor).unwrap();
        self.b.append_data(&mut h, path, std::io::empty()).unwrap();
        self
    }

    pub fn finish(self) -> Vec<u8> {
        self.b.into_inner().unwrap()
    }
}
