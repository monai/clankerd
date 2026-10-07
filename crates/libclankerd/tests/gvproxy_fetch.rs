//! gvproxy is downloaded from a pinned URL, verified against a pinned sha256
//! and cached. A local HTTP server stands in for GitHub.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use libclankerd::ErrorKind;
use libclankerd::net::GvproxyFetcher;

/// Serves `body` for every request until the returned guard is dropped.
struct Server {
    url: String,
    hits: Arc<AtomicUsize>,
}

fn serve(body: &'static [u8]) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/gvproxy-darwin", listener.local_addr().unwrap());
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    std::thread::spawn(move || {
        for mut conn in listener.incoming().flatten() {
            let mut buf = [0u8; 4096];
            let _ = conn.read(&mut buf);
            counter.fetch_add(1, Ordering::SeqCst);
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = conn.write_all(head.as_bytes());
            let _ = conn.write_all(body);
        }
    });
    Server { url, hits }
}

/// sha256("fake gvproxy\n"), computed with `sha256sum`.
const BODY: &[u8] = b"fake gvproxy\n";
const BODY_SHA: &str = "ff5101479ef01561fa85dec84bb054fe39b5b4e8a5dc10db2a9147a437919b8f";

fn fetcher(dir: &std::path::Path, server: &Server, sha: &str) -> GvproxyFetcher {
    GvproxyFetcher::new(dir, "v-test", &server.url, sha)
}

#[test]
fn a_download_matching_the_pinned_checksum_is_cached_and_executable() {
    let dir = tempfile::tempdir().unwrap();
    let server = serve(BODY);
    let path = fetcher(dir.path(), &server, BODY_SHA).ensure().unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), BODY);
    assert_ne!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o111,
        0
    );
    assert!(path.starts_with(dir.path()));

    // Cached: no second download.
    let again = fetcher(dir.path(), &server, BODY_SHA).ensure().unwrap();
    assert_eq!(again, path);
    assert_eq!(server.hits.load(Ordering::SeqCst), 1);
}

#[test]
fn a_download_with_the_wrong_checksum_is_rejected_and_not_kept() {
    let dir = tempfile::tempdir().unwrap();
    let server = serve(b"tampered\n");
    let err = fetcher(dir.path(), &server, BODY_SHA).ensure().unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Unavailable);
    assert!(err.message().contains("checksum"), "{}", err.message());
    let left: Vec<_> = walk(dir.path());
    assert!(left.is_empty(), "files left behind: {left:?}");
}

#[test]
fn a_cached_file_that_no_longer_matches_is_downloaded_again() {
    let dir = tempfile::tempdir().unwrap();
    let server = serve(BODY);
    let path = fetcher(dir.path(), &server, BODY_SHA).ensure().unwrap();
    std::fs::write(&path, b"corrupted").unwrap();
    let path = fetcher(dir.path(), &server, BODY_SHA).ensure().unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), BODY);
    assert_eq!(server.hits.load(Ordering::SeqCst), 2);
}

#[test]
fn the_pinned_release_names_a_darwin_asset_and_a_sha256() {
    let p = GvproxyFetcher::pinned(std::path::Path::new("/nonexistent"));
    assert!(
        p.url()
            .starts_with("https://github.com/containers/gvisor-tap-vsock/releases/download/")
    );
    assert!(p.url().contains(p.version()));
    assert_eq!(p.sha256().len(), 64);
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        if e.path().is_dir() {
            out.extend(walk(&e.path()));
        } else {
            out.push(e.path());
        }
    }
    out
}
