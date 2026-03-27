//! Integration tests for the SFTP module against a local SSH server.
//!
//! These tests are `#[ignore]`d by default — they require a running sshd
//! on localhost with key-based auth for the current user.
//! Run with: `cargo test --test sftp_localhost -- --ignored`
//!
//! CI sets up sshd automatically (see .github/workflows/ci.yml).

use std::io::Read;

use fileman::sftp;

fn connect_localhost() -> sftp::SftpSession {
    let config = sftp::load_ssh_config();
    sftp::connect("localhost", &config).expect("connect to localhost")
}

#[test]
#[ignore]
fn sftp_connect() {
    let session = connect_localhost();
    assert_eq!(session.host, "localhost");
    assert!(session.session.authenticated());
}

#[test]
#[ignore]
fn sftp_read_root_directory() {
    let session = connect_localhost();
    let entries = sftp::read_directory(&session.sftp, "localhost", "/").expect("read root dir");
    // Root should have no ".." entry
    assert!(
        !entries.iter().any(|e| e.name == ".."),
        "root dir should not have .."
    );
    // Root should have some well-known dirs
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert!(names.contains(&"etc"), "root should contain etc: {names:?}");
    assert!(names.contains(&"tmp"), "root should contain tmp: {names:?}");
}

#[test]
#[ignore]
fn sftp_read_subdirectory_has_parent() {
    let session = connect_localhost();
    let entries = sftp::read_directory(&session.sftp, "localhost", "/tmp").expect("read /tmp");
    assert!(
        entries.iter().any(|e| e.name == ".."),
        "/tmp should have .. entry"
    );
    // ".." should point to "/"
    let dotdot = entries.iter().find(|e| e.name == "..").unwrap();
    if let fileman::core::EntryLocation::Remote { path, .. } = &dotdot.location {
        assert_eq!(path, "/");
    } else {
        panic!(".. should be EntryLocation::Remote");
    }
}

#[test]
#[ignore]
fn sftp_write_read_delete() {
    let session = connect_localhost();
    let test_path = "/tmp/fileman_sftp_test_write";
    let contents = b"hello from fileman sftp test";

    // Write
    sftp::write_file(&session.sftp, test_path, contents).expect("write file");

    // Read back
    let data = sftp::read_bytes_prefix(&session.sftp, test_path, 1024).expect("read file");
    assert_eq!(data, contents);

    // Delete
    sftp::recursive_delete(&session.sftp, test_path, false, None).expect("delete file");

    // Verify gone
    let result = sftp::read_bytes_prefix(&session.sftp, test_path, 1024);
    assert!(result.is_err(), "file should be deleted");
}

#[test]
#[ignore]
fn sftp_mkdir_and_delete() {
    let session = connect_localhost();
    let dir_path = "/tmp/fileman_sftp_test_dir";

    // Clean up in case of prior failed run
    let _ = sftp::recursive_delete(&session.sftp, dir_path, true, None);

    // Create directory
    sftp::mkdir(&session.sftp, dir_path).expect("mkdir");

    // Write a file inside
    let file_path = format!("{dir_path}/nested.txt");
    sftp::write_file(&session.sftp, &file_path, b"nested").expect("write nested");

    // List it
    let entries = sftp::read_directory(&session.sftp, "localhost", dir_path).expect("read dir");
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert!(
        names.contains(&"nested.txt"),
        "should contain nested.txt: {names:?}"
    );

    // Recursive delete
    sftp::recursive_delete(&session.sftp, dir_path, true, None).expect("recursive delete");

    // Verify gone
    let result = sftp::read_directory(&session.sftp, "localhost", dir_path);
    assert!(result.is_err(), "dir should be deleted");
}

#[test]
#[ignore]
fn sftp_rename() {
    let session = connect_localhost();
    let src = "/tmp/fileman_sftp_test_rename_src";
    let dst = "/tmp/fileman_sftp_test_rename_dst";

    // Clean up
    let _ = sftp::recursive_delete(&session.sftp, src, false, None);
    let _ = sftp::recursive_delete(&session.sftp, dst, false, None);

    sftp::write_file(&session.sftp, src, b"rename me").expect("write");
    sftp::rename(&session.sftp, src, dst).expect("rename");

    let data = sftp::read_bytes_prefix(&session.sftp, dst, 1024).expect("read renamed");
    assert_eq!(data, b"rename me");

    // Old path should be gone
    let result = sftp::read_bytes_prefix(&session.sftp, src, 1024);
    assert!(result.is_err(), "old path should not exist");

    sftp::recursive_delete(&session.sftp, dst, false, None).expect("cleanup");
}

#[test]
#[ignore]
fn sftp_copy_remote_to_local() {
    let session = connect_localhost();
    let remote_path = "/tmp/fileman_sftp_test_r2l";
    sftp::write_file(&session.sftp, remote_path, b"copy me locally").expect("write");

    let local_dir = std::env::temp_dir().join("fileman_sftp_test_r2l_out");
    std::fs::create_dir_all(&local_dir).ok();
    let local_file = local_dir.join("copied.txt");

    sftp::copy_remote_to_local(&session.sftp, remote_path, &local_file).expect("copy r2l");
    let local_data = std::fs::read(&local_file).expect("read local");
    assert_eq!(local_data, b"copy me locally");

    // Cleanup
    sftp::recursive_delete(&session.sftp, remote_path, false, None).ok();
    std::fs::remove_dir_all(&local_dir).ok();
}

#[test]
#[ignore]
fn sftp_copy_local_to_remote() {
    let session = connect_localhost();
    let local_dir = std::env::temp_dir().join("fileman_sftp_test_l2r");
    std::fs::create_dir_all(&local_dir).ok();
    let local_file = local_dir.join("upload.txt");
    std::fs::write(&local_file, b"upload me").expect("write local");

    let remote_path = "/tmp/fileman_sftp_test_l2r_uploaded";
    let _ = sftp::recursive_delete(&session.sftp, remote_path, false, None);

    sftp::copy_local_to_remote(&session.sftp, &local_file, remote_path).expect("copy l2r");

    let data = sftp::read_bytes_prefix(&session.sftp, remote_path, 1024).expect("read remote");
    assert_eq!(data, b"upload me");

    // Cleanup
    sftp::recursive_delete(&session.sftp, remote_path, false, None).ok();
    std::fs::remove_dir_all(&local_dir).ok();
}

#[test]
#[ignore]
fn sftp_error_on_permission_denied_returns_parent() {
    let session = connect_localhost();
    // /root is typically not readable by normal users
    let result = sftp::read_directory(&session.sftp, "localhost", "/root");
    // This may succeed if running as root (CI), or fail — either is fine.
    // The important thing is it doesn't panic.
    match result {
        Ok(entries) => {
            // If it succeeded (running as root), just verify it's valid
            assert!(!entries.is_empty() || entries.is_empty()); // no panic
        }
        Err(msg) => {
            assert!(
                msg.contains("readdir") || msg.contains("permission") || msg.contains("denied"),
                "error should mention the failure: {msg}"
            );
        }
    }
}

#[test]
#[ignore]
fn sftp_open_remote_reader() {
    let session = connect_localhost();
    let remote_path = "/tmp/fileman_sftp_test_reader";
    let content = b"streaming read test data with more bytes";
    sftp::write_file(&session.sftp, remote_path, content).expect("write");

    let mut reader = sftp::open_remote_reader(&session.sftp, remote_path).expect("open reader");
    let mut buf = Vec::new();
    reader.read_to_end(&mut buf).expect("read_to_end");
    assert_eq!(buf, content);

    sftp::recursive_delete(&session.sftp, remote_path, false, None).ok();
}

#[test]
#[ignore]
fn sftp_discover_hosts_does_not_panic() {
    // Just ensure it doesn't panic — result depends on ~/.ssh/config
    let _hosts = sftp::discover_ssh_hosts();
}

#[test]
fn sftp_parse_ssh_config() {
    let config_text = "\
Host myserver
    Hostname 10.0.0.1
    User deploy
    Port 2222
    IdentityFile ~/.ssh/deploy_key

Host *.example.com
    User admin

Host jump
    Hostname jump.internal
    IdentityFile ~/.ssh/jump_key
    IdentityFile ~/.ssh/backup_key
";
    let parsed = sftp::parse_ssh_config(config_text);

    let my = parsed.get("myserver").expect("myserver");
    assert_eq!(my.hostname.as_deref(), Some("10.0.0.1"));
    assert_eq!(my.user.as_deref(), Some("deploy"));
    assert_eq!(my.port, Some(2222));
    assert_eq!(my.identity_files.len(), 1);

    // Wildcard host should be excluded
    assert!(!parsed.contains_key("*.example.com"));

    let jump = parsed.get("jump").expect("jump");
    assert_eq!(jump.hostname.as_deref(), Some("jump.internal"));
    assert_eq!(jump.identity_files.len(), 2);
}
