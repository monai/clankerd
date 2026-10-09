mod common;

use libclankerd::{DiskPopulator, ErrorKind, VmspawnPopulator};
use std::fs::File;
use std::io::{Seek, SeekFrom, Write};

fn superblock(blocks: u64, log_block_size: u32, wide: bool) -> [u8; 1024] {
    let mut bytes = [0; 1024];
    bytes[4..8].copy_from_slice(&(blocks as u32).to_le_bytes());
    bytes[24..28].copy_from_slice(&log_block_size.to_le_bytes());
    bytes[56..58].copy_from_slice(&[0x53, 0xef]);
    if wide {
        bytes[96..100].copy_from_slice(&0x80u32.to_le_bytes());
        bytes[336..340].copy_from_slice(&((blocks >> 32) as u32).to_le_bytes());
    }
    bytes
}

#[test]
fn cached_disks_require_enough_bytes_for_the_declared_filesystem() {
    let populator = VmspawnPopulator::new("fake-helper", "fake-boot-dir");
    for (blocks, log_size, wide, capacity) in [
        (8192, 0, false, 8 * 1024 * 1024),
        (8192, 1, false, 16 * 1024 * 1024),
        (908032, 2, false, 3_719_299_072),
        ((1u64 << 32) + 1, 0, true, 4_398_046_512_128),
    ] {
        let dir = common::Env::new();
        let disk = dir.root().join("disk.ext4");
        let mut file = File::create(&disk).unwrap();
        file.seek(SeekFrom::Start(1024)).unwrap();
        file.write_all(&superblock(blocks, log_size, wide)).unwrap();
        for missing in [1, 4096, 65536] {
            file.set_len(capacity - missing).unwrap();
            assert!(!populator.cached_disk_valid(&disk).unwrap());
        }
        for extra in [0, 4096] {
            file.set_len(capacity + extra).unwrap();
            assert!(populator.cached_disk_valid(&disk).unwrap());
        }
    }
}

#[test]
fn cached_disks_reject_invalid_filesystem_geometry() {
    let populator = VmspawnPopulator::new("fake-helper", "fake-boot-dir");
    let mut bad_magic = superblock(8192, 2, false);
    bad_magic[56] = 0;
    for bytes in [
        bad_magic,
        superblock(0, 2, false),
        superblock(8192, 7, false),
        superblock(8192, 32, false),
        superblock(u64::MAX, 2, true),
    ] {
        let dir = common::Env::new();
        let disk = dir.root().join("disk.ext4");
        let mut file = File::create(&disk).unwrap();
        file.seek(SeekFrom::Start(1024)).unwrap();
        file.write_all(&bytes).unwrap();
        assert_eq!(
            populator.cached_disk_valid(&disk).unwrap_err().kind(),
            ErrorKind::System
        );
    }
}
