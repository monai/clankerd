//! A tiny in-process OCI registry (plain HTTP, anonymous) serving fixed images,
//! so pull/merge/cache tests run offline. It speaks just enough of the
//! distribution API for oci-client: `/v2/`, manifests and blobs.
#![allow(dead_code)]

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

use sha2::{Digest, Sha256};

type Routes = Arc<Mutex<HashMap<String, (String, Vec<u8>)>>>;

pub struct Registry {
    pub addr: String,
    routes: Routes,
}

pub fn sha256(bytes: &[u8]) -> String {
    let d = Sha256::digest(bytes);
    format!(
        "sha256:{}",
        d.iter().map(|b| format!("{b:02x}")).collect::<String>()
    )
}

/// A layer: gzip'd tar of `(path, contents)` regular files, plus whiteouts as
/// empty files named `.wh.<name>`.
pub fn layer(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut b = tar::Builder::new(Vec::new());
    for (path, data) in files {
        let mut h = tar::Header::new_gnu();
        h.set_size(data.len() as u64);
        h.set_mode(0o644);
        h.set_mtime(1_700_000_000);
        h.set_uid(0);
        h.set_gid(0);
        b.append_data(&mut h, path, *data).unwrap();
    }
    let tar = b.into_inner().unwrap();
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    gz.write_all(&tar).unwrap();
    gz.finish().unwrap()
}

impl Registry {
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let routes: Routes = Arc::default();
        let r = routes.clone();
        std::thread::spawn(move || {
            for conn in listener.incoming().flatten() {
                let r = r.clone();
                std::thread::spawn(move || handle(conn, r));
            }
        });
        Registry { addr, routes }
    }

    /// Publishes an image (linux/arm64) built from gzip'd `layers` as
    /// `repo:tag` and returns its manifest digest.
    pub fn push(&self, repo: &str, tag: &str, layers: &[Vec<u8>]) -> String {
        self.push_with_config(repo, tag, layers, serde_json::json!({}))
    }

    /// Like [`Registry::push`], with the image config's `config` object
    /// (`Entrypoint`, `Cmd`, `Env`, `User`, `WorkingDir`).
    pub fn push_with_config(
        &self,
        repo: &str,
        tag: &str,
        layers: &[Vec<u8>],
        runtime: serde_json::Value,
    ) -> String {
        let config = serde_json::to_vec(&serde_json::json!({
            "architecture": "arm64",
            "os": "linux",
            "config": runtime,
            "rootfs": {"type": "layers", "diff_ids": []},
        }))
        .unwrap();
        let mut descriptors = Vec::new();
        for l in layers {
            let digest = sha256(l);
            self.blob(repo, &digest, l.clone());
            descriptors.push(serde_json::json!({
                "mediaType": "application/vnd.oci.image.layer.v1.tar+gzip",
                "digest": digest,
                "size": l.len(),
            }));
        }
        let config_digest = sha256(&config);
        self.blob(repo, &config_digest, config.clone());
        let manifest = serde_json::to_vec(&serde_json::json!({
            "schemaVersion": 2,
            "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "config": {
                "mediaType": "application/vnd.oci.image.config.v1+json",
                "digest": config_digest,
                "size": config.len(),
            },
            "layers": descriptors,
        }))
        .unwrap();
        let digest = sha256(&manifest);
        let ct = "application/vnd.oci.image.manifest.v1+json".to_string();
        let mut routes = self.routes.lock().unwrap();
        routes.insert(
            format!("/v2/{repo}/manifests/{tag}"),
            (ct.clone(), manifest.clone()),
        );
        routes.insert(format!("/v2/{repo}/manifests/{digest}"), (ct, manifest));
        digest
    }

    fn blob(&self, repo: &str, digest: &str, bytes: Vec<u8>) {
        self.routes.lock().unwrap().insert(
            format!("/v2/{repo}/blobs/{digest}"),
            ("application/octet-stream".into(), bytes),
        );
    }
}

fn handle(conn: TcpStream, routes: Routes) {
    let mut reader = BufReader::new(conn.try_clone().unwrap());
    let mut line = String::new();
    if reader.read_line(&mut line).unwrap_or(0) == 0 {
        return;
    }
    let mut parts = line.split_whitespace();
    let (method, path) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    let (method, path) = (method.to_owned(), path.to_owned());
    loop {
        let mut h = String::new();
        if reader.read_line(&mut h).unwrap_or(0) == 0 || h.trim().is_empty() {
            break;
        }
    }
    let mut out = conn;
    let found = if path == "/v2/" {
        Some(("application/json".to_string(), b"{}".to_vec()))
    } else {
        routes.lock().unwrap().get(&path).cloned()
    };
    match found {
        Some((ct, body)) => {
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {ct}\r\nContent-Length: {}\r\nDocker-Content-Digest: {}\r\nConnection: close\r\n\r\n",
                body.len(),
                sha256(&body)
            );
            let _ = out.write_all(head.as_bytes());
            if method != "HEAD" {
                let _ = out.write_all(&body);
            }
        }
        None => {
            let _ = out.write_all(
                b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
        }
    }
    let _ = out.flush();
    let mut sink = Vec::new();
    let _ = reader.read_to_end(&mut sink);
}
