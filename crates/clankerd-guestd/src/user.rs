//! Docker-style `user[:group]` resolution against passwd/group file contents.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub uid: u32,
    pub gid: u32,
    /// Supplementary groups (only when no explicit group was requested).
    pub groups: Vec<u32>,
    pub name: Option<String>,
    pub home: Option<String>,
}

struct Passwd<'a> {
    name: &'a str,
    uid: u32,
    gid: u32,
    home: &'a str,
}

fn passwd_entries(passwd: &str) -> impl Iterator<Item = Passwd<'_>> {
    passwd.lines().filter_map(|l| {
        let f: Vec<&str> = l.split(':').collect();
        if f.len() < 6 || l.starts_with('#') {
            return None;
        }
        Some(Passwd {
            name: f[0],
            uid: f[2].parse().ok()?,
            gid: f[3].parse().ok()?,
            home: f[5],
        })
    })
}

/// `(name, gid, members)` of each group line.
fn group_entries(group: &str) -> impl Iterator<Item = (&str, u32, Vec<&str>)> {
    group.lines().filter_map(|l| {
        let f: Vec<&str> = l.split(':').collect();
        if f.len() < 4 || l.starts_with('#') {
            return None;
        }
        Some((
            f[0],
            f[2].parse().ok()?,
            f[3].split(',').filter(|m| !m.is_empty()).collect(),
        ))
    })
}

/// Resolves `spec` (`name|uid[:name|gid]`). An empty spec is the caller's concern.
pub fn resolve(spec: &str, passwd: &str, group: &str) -> Result<Resolved, String> {
    let (user, grp) = match spec.split_once(':') {
        Some((u, g)) => (u, Some(g)),
        None => (spec, None),
    };
    if user.is_empty() {
        return Err(format!("invalid user spec {spec:?}"));
    }
    let entry = match user.parse::<u32>() {
        Ok(uid) => passwd_entries(passwd).find(|p| p.uid == uid),
        Err(_) => Some(
            passwd_entries(passwd)
                .find(|p| p.name == user)
                .ok_or_else(|| {
                    format!("unable to find user {user}: no matching entry in passwd file")
                })?,
        ),
    };
    let uid = match (&entry, user.parse::<u32>()) {
        (Some(p), _) => p.uid,
        (None, Ok(uid)) => uid,
        (None, Err(_)) => unreachable!("named users were resolved above"),
    };
    let mut groups = Vec::new();
    let gid = match grp {
        Some(g) => match g.parse::<u32>() {
            Ok(gid) => gid,
            Err(_) => group_entries(group)
                .find(|(n, _, _)| *n == g)
                .map(|(_, gid, _)| gid)
                .ok_or_else(|| format!("no matching entries in group file for {g}"))?,
        },
        None => {
            if let Some(p) = &entry {
                groups = group_entries(group)
                    .filter(|(_, _, members)| members.contains(&p.name))
                    .map(|(_, gid, _)| gid)
                    .collect();
            }
            entry.as_ref().map_or(0, |p| p.gid)
        }
    };
    Ok(Resolved {
        uid,
        gid,
        groups,
        name: entry.as_ref().map(|p| p.name.to_owned()),
        home: entry.as_ref().map(|p| p.home.to_owned()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const PASSWD: &str =
        "root:x:0:0:root:/root:/bin/sh\nalice:x:1000:100:Alice:/home/alice:/bin/sh\n";
    const GROUP: &str = "root:x:0:\nusers:x:100:\nwheel:x:10:alice\naudio:x:29:alice,bob\n";

    #[test]
    fn name_resolves_to_ids_home_and_supplementary_groups() {
        let r = resolve("alice", PASSWD, GROUP).unwrap();
        assert_eq!((r.uid, r.gid), (1000, 100));
        assert_eq!(r.groups, vec![10, 29]);
        assert_eq!(r.home.as_deref(), Some("/home/alice"));
    }

    #[test]
    fn numeric_uid_without_entry_gets_gid_zero() {
        let r = resolve("4242", PASSWD, GROUP).unwrap();
        assert_eq!((r.uid, r.gid), (4242, 0));
        assert!(r.groups.is_empty() && r.home.is_none());
    }

    #[test]
    fn explicit_group_overrides_and_drops_supplementary() {
        let r = resolve("alice:wheel", PASSWD, GROUP).unwrap();
        assert_eq!((r.uid, r.gid), (1000, 10));
        assert!(r.groups.is_empty());
        assert_eq!(resolve("1000:7", PASSWD, GROUP).unwrap().gid, 7);
    }

    #[test]
    fn unknown_names_are_errors() {
        assert!(resolve("nobody-here", PASSWD, GROUP).is_err());
        assert!(resolve("alice:nogroup", PASSWD, GROUP).is_err());
        assert!(resolve(":1", PASSWD, GROUP).is_err());
    }
}
