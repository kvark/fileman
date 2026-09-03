//! `known_hosts` verification.
//!
//! Kept in-tree rather than taken from `sunset-stdasync` because the policy
//! matters and differs: hashed (`|1|salt|hash`) entries have to be understood,
//! since `ssh-keyscan -H` and OpenSSH's `HashKnownHosts` produce them and
//! missing one would silently downgrade a pinned host to trust-on-first-use.
//! New hosts are pinned on first sight, and anything the check cannot decide
//! fails closed.

use std::io::Write as _;

use base64::Engine as _;
use hmac::Mac as _;
use sunset::packets::PubKey;
use sunset::sshwire;

/// Result of looking a host up in `known_hosts`.
enum Lookup {
    Match,
    Mismatch,
    NotFound,
}

fn known_hosts_path() -> Option<std::path::PathBuf> {
    crate::sftp::home_dir().map(|h| h.join(".ssh").join("known_hosts"))
}

/// The host as it is written in `known_hosts`: non-default ports are bracketed.
fn host_pattern(host: &str, port: u16) -> String {
    let host = host.to_lowercase();
    if port == 22 {
        host
    } else {
        format!("[{host}]:{port}")
    }
}

/// Serialises a public key the way `known_hosts` stores it: the algorithm name
/// and the base64 of the SSH wire encoding.
fn encode_key(key: &PubKey) -> Result<(String, String), String> {
    let algo = key
        .algorithm_name()
        .map_err(|_| "unrecognised host key algorithm".to_string())?
        .to_string();
    let mut blob = Vec::new();
    sshwire::ssh_push_vec(&mut blob, key).map_err(|e| format!("encoding host key: {e}"))?;
    Ok((
        algo,
        base64::engine::general_purpose::STANDARD.encode(&blob),
    ))
}

/// Does one `known_hosts` host field name this host?
///
/// Handles plain names, comma-separated lists, and hashed entries. Negations
/// (`!host`) exclude the entry.
fn host_matches(field: &str, want: &str) -> bool {
    if let Some(rest) = field.strip_prefix("|1|") {
        // |1|<base64 salt>|<base64 hash>, hash = HMAC-SHA1(key = salt, host)
        let Some((salt_b64, hash_b64)) = rest.split_once('|') else {
            return false;
        };
        let engine = base64::engine::general_purpose::STANDARD;
        let (Ok(salt), Ok(hash)) = (engine.decode(salt_b64), engine.decode(hash_b64)) else {
            return false;
        };
        let Ok(mut mac) = hmac::Hmac::<sha1::Sha1>::new_from_slice(&salt) else {
            return false;
        };
        mac.update(want.as_bytes());
        return mac.verify_slice(&hash).is_ok();
    }
    let mut matched = false;
    for pat in field.split(',') {
        match pat.strip_prefix('!') {
            // A negation anywhere wins.
            Some(neg) if neg.eq_ignore_ascii_case(want) => return false,
            Some(_) => (),
            None if pat.eq_ignore_ascii_case(want) => matched = true,
            None => (),
        }
    }
    matched
}

fn lookup(contents: &str, want: &str, algo: &str, key_b64: &str) -> Lookup {
    let mut found_host = false;
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // Optional marker (@cert-authority, @revoked) comes first.
        let rest = match line.strip_prefix('@') {
            Some(r) => match r.split_once(' ') {
                // Markers change the meaning of the entry; we do not implement
                // them, so skip rather than misread one as a plain pin.
                Some(_) => continue,
                None => continue,
            },
            None => line,
        };
        let mut fields = rest.split_whitespace();
        let (Some(hosts), Some(line_algo), Some(line_key)) =
            (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        if !host_matches(hosts, want) {
            continue;
        }
        found_host = true;
        if line_algo == algo && line_key == key_b64 {
            return Lookup::Match;
        }
    }
    // A different key of the same type, or only other types, both mean the pin
    // we hold does not cover this key.
    if found_host {
        Lookup::Mismatch
    } else {
        Lookup::NotFound
    }
}

/// Checks the server's host key, pinning it on first sight.
///
/// Returns `Err` with a message to show the user when the connection must not
/// proceed.
pub fn verify(host: &str, port: u16, key: &PubKey) -> Result<(), String> {
    let (algo, key_b64) = encode_key(key)?;
    let want = host_pattern(host, port);

    let Some(path) = known_hosts_path() else {
        return Err("Cannot locate the home directory to check known_hosts. \
                    Refusing to connect rather than skip host-key verification."
            .into());
    };

    let contents = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        // If the file exists but cannot be read, treating it as empty would
        // downgrade every pinned host to trust-on-first-use, so fail closed.
        Err(e) => {
            return Err(format!(
                "Could not read {}: {e}. Refusing to connect rather than skip \
                 host-key verification.",
                path.display()
            ));
        }
    };

    match lookup(&contents, &want, &algo, &key_b64) {
        Lookup::Match => Ok(()),
        Lookup::Mismatch => Err(format!(
            "HOST KEY MISMATCH for {host}! The server's key has changed. \
             This could indicate a man-in-the-middle attack. \
             If the key change is expected, remove the old entry from {}.",
            path.display()
        )),
        Lookup::NotFound => {
            log::warn!("Host key for {host} not found in known_hosts; accepting (TOFU).");
            if let Err(e) = append_entry(&path, &want, &algo, &key_b64) {
                // Failing to record the pin is not fatal to this connection,
                // but every later one would be trust-on-first-use again.
                log::warn!("Could not record host key for {host}: {e}");
            }
            Ok(())
        }
    }
}

fn append_entry(
    path: &std::path::Path,
    host: &str,
    algo: &str,
    key_b64: &str,
) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    // A file that does not end in a newline would otherwise get its last entry
    // merged with ours.
    let needs_newline = std::fs::metadata(path)
        .map(|m| m.len() > 0)
        .unwrap_or(false)
        && !ends_with_newline(path);
    if needs_newline {
        f.write_all(b"\n")?;
    }
    writeln!(f, "{host} {algo} {key_b64}")
}

fn ends_with_newline(path: &std::path::Path) -> bool {
    use std::io::{Read as _, Seek as _};
    let Ok(mut f) = std::fs::File::open(path) else {
        return true;
    };
    if f.seek(std::io::SeekFrom::End(-1)).is_err() {
        return true;
    }
    let mut b = [0u8; 1];
    matches!(f.read_exact(&mut b), Ok(())) && b[0] == b'\n'
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIExampleExampleExampleExampleExampleEx";
    const ALGO: &str = "ssh-ed25519";

    #[test]
    fn plain_host_matches() {
        assert!(host_matches("example.com", "example.com"));
        assert!(host_matches("EXAMPLE.com", "example.com"));
        assert!(!host_matches("other.com", "example.com"));
    }

    #[test]
    fn comma_list_and_negation() {
        assert!(host_matches("a.com,b.com", "b.com"));
        assert!(!host_matches("a.com,!b.com", "b.com"));
    }

    #[test]
    fn hashed_host_matches() {
        // Entry produced the way ssh-keyscan -H writes them.
        let salt = b"0123456789abcdefffff";
        let mut mac = hmac::Hmac::<sha1::Sha1>::new_from_slice(salt).unwrap();
        mac.update(b"example.com");
        let engine = base64::engine::general_purpose::STANDARD;
        let field = format!(
            "|1|{}|{}",
            engine.encode(salt),
            engine.encode(mac.finalize().into_bytes())
        );
        assert!(host_matches(&field, "example.com"));
        assert!(!host_matches(&field, "other.com"));
    }

    #[test]
    fn lookup_reports_match_mismatch_and_absence() {
        let file = format!("example.com {ALGO} {KEY}\n");
        assert!(matches!(
            lookup(&file, "example.com", ALGO, KEY),
            Lookup::Match
        ));
        assert!(matches!(
            lookup(&file, "example.com", ALGO, "AAAAdifferent"),
            Lookup::Mismatch
        ));
        assert!(matches!(
            lookup(&file, "unknown.com", ALGO, KEY),
            Lookup::NotFound
        ));
    }

    #[test]
    fn comments_and_markers_are_skipped() {
        let file = format!("# a comment\n@revoked example.com {ALGO} {KEY}\n");
        // The revoked marker must not read as a pin for this key.
        assert!(matches!(
            lookup(&file, "example.com", ALGO, KEY),
            Lookup::NotFound
        ));
    }

    #[test]
    fn non_default_port_is_bracketed() {
        assert_eq!(host_pattern("example.com", 22), "example.com");
        assert_eq!(host_pattern("example.com", 2222), "[example.com]:2222");
    }
}
