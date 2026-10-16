//! Value normalization shared by command definitions.
use libclankerd::{Mount, SocketBinding};
use std::path::{Component, PathBuf};

pub fn size(s: &str) -> Result<u64, String> {
    libclankerd::parse_size(s).map_err(|e| e.to_string())
}
pub fn guest_path(s: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(s);
    if !path.is_absolute()
        || s == "/"
        || s.split('/').any(|p| p == "." || p == "..")
        || path
            .components()
            .any(|c| !matches!(c, Component::RootDir | Component::Normal(_)))
    {
        return Err("guest target must be absolute, non-root, without . or .. components".into());
    }
    Ok(path)
}
fn mount(kind: &str, source: &str, target: &str, readonly: bool) -> Result<Mount, String> {
    let target = guest_path(target)?;
    if source.is_empty() {
        return Err("mount source cannot be empty".into());
    }
    match kind {
        "bind" => {
            let expanded = if let Some(rest) = source.strip_prefix("~/") {
                PathBuf::from(std::env::var_os("HOME").ok_or("HOME is required for ~/ paths")?)
                    .join(rest)
            } else {
                PathBuf::from(source)
            };
            let absolute = std::path::absolute(expanded).map_err(|e| e.to_string())?;
            let source = absolute.canonicalize().unwrap_or(absolute);
            Ok(Mount::Bind {
                source,
                target,
                read_only: readonly,
            })
        }
        "volume" if readonly => Err("read-only named-volume attachments are not supported".into()),
        "volume" => Ok(Mount::volume(source, target)),
        _ => Err("mount type must be bind or volume".into()),
    }
}
pub fn parse_mount(s: &str) -> Result<Mount, String> {
    let parts: Vec<_> = s.split(':').collect();
    let (source, target, readonly) =
        match parts.as_slice() {
            [s, t] | [s, t, "rw"] => (*s, *t, false),
            [s, t, "ro"] => (*s, *t, true),
            _ => return Err(
                "expected SOURCE:/GUEST[:ro|rw]; create sized volumes with volume create --size"
                    .into(),
            ),
        };
    let kind = if source.starts_with('/')
        || source.starts_with("./")
        || source.starts_with("../")
        || source.starts_with("~/")
    {
        "bind"
    } else {
        "volume"
    };
    mount(kind, source, target, readonly)
}
fn fields(s: &str) -> Result<std::collections::BTreeMap<&str, &str>, String> {
    let mut fields = std::collections::BTreeMap::new();
    for field in s.split(',') {
        let (key, value) = match field.split_once('=') {
            Some(pair) => pair,
            None if field == "readonly" => (field, "true"),
            None => return Err(format!("field {field} requires a value")),
        };
        if fields.insert(key, value).is_some() {
            return Err(format!("repeated field {key}"));
        }
    }
    Ok(fields)
}
pub fn typed_mount(s: &str) -> Result<Mount, String> {
    let mut f = fields(s)?;
    let kind = f.remove("type").ok_or("mount requires type")?;
    let source = f.remove("source").ok_or("mount requires source")?;
    let target = f.remove("target").ok_or("mount requires target")?;
    let readonly = match f.remove("readonly") {
        None | Some("false") => false,
        Some("true") => true,
        _ => return Err("readonly must be true or false".into()),
    };
    if !f.is_empty() {
        return Err(format!("unknown mount fields: {:?}", f.keys()));
    }
    mount(kind, source, target, readonly)
}
pub fn socket(s: &str) -> Result<SocketBinding, String> {
    let mut f = fields(s)?;
    let host_path = guest_path(f.remove("host").ok_or("socket requires host=/HOST")?)?;
    let guest_path = guest_path(f.remove("guest").ok_or("socket requires guest=/GUEST")?)?;
    if !f.is_empty() {
        return Err("unknown socket fields".into());
    }
    Ok(SocketBinding {
        host_path,
        guest_path,
    })
}
