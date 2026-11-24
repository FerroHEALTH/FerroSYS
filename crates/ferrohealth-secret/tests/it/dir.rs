// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! A directory of named secrets: a plain directory, and the atomic `..data`
//! symlink swap the kubelet writes a mounted Secret with.

use std::error::Error as _;
use std::fs;
use std::os::unix::fs::{PermissionsExt as _, symlink};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use ferrohealth_secret::dir::{PollSeconds, SecretDir, SecretDirConfig, SecretDirError};
use secrecy::ExposeSecret as _;

const SENTINEL: &str = "Qz7sentinel";

fn value(dir: &SecretDir, name: &str) -> String {
    dir.get(name)
        .expect("the secret reads")
        .expose_secret()
        .to_owned()
}

/// Publishes one update of a mounted Secret as the kubelet's atomic writer
/// does: the keys into a new hidden directory, a `..data_tmp` symlink to it
/// renamed over `..data`, a top-level `<key> -> ..data/<key>` link for each
/// new key and none for a removed one, then the old directory removed.
fn publish(root: &Path, generation: usize, keys: &[(&str, &str)]) {
    let name = format!("..2026_11_24_gen_{generation}");
    let fresh = root.join(&name);
    fs::create_dir_all(&fresh).expect("the update directory is made");
    for (key, content) in keys {
        fs::write(fresh.join(key), content).expect("the key is written");
    }
    let previous = fs::read_link(root.join("..data")).ok();
    let staged = root.join("..data_tmp");
    symlink(&name, &staged).expect("the staged link is made");
    fs::rename(&staged, root.join("..data")).expect("the link swaps");
    for (key, _) in keys {
        let link = root.join(key);
        if fs::symlink_metadata(&link).is_err() {
            symlink(Path::new("..data").join(key), &link).expect("the key link is made");
        }
    }
    for item in fs::read_dir(root).expect("the root lists") {
        let item = item.expect("an entry");
        let file = item.file_name().into_string().expect("a UTF-8 name");
        if !file.starts_with('.') && !keys.iter().any(|(key, _)| *key == file) {
            fs::remove_file(item.path()).expect("a removed key's link goes");
        }
    }
    if let Some(previous) = previous {
        fs::remove_dir_all(root.join(previous)).expect("the old update goes");
    }
}

/// The eight keys of one update, every one holding the update's number.
fn update(generation: usize) -> Vec<(String, String)> {
    (0..8)
        .map(|key| (format!("key{key}"), format!("gen-{generation}\n")))
        .collect()
}

fn publish_update(root: &Path, generation: usize) {
    let keys = update(generation);
    let keys: Vec<(&str, &str)> = keys
        .iter()
        .map(|(key, content)| (key.as_str(), content.as_str()))
        .collect();
    publish(root, generation, &keys);
}

#[test]
fn a_plain_directory_resolves_by_name_and_lists_without_values() {
    let root = tempfile::tempdir().expect("a directory");
    fs::write(root.path().join("hmac"), format!("{SENTINEL}\n")).expect("written");
    fs::write(root.path().join("client.crt"), "CERT\n").expect("written");
    fs::write(
        root.path().join("client.key"),
        format!("{SENTINEL}-key\r\n"),
    )
    .expect("written");
    fs::write(root.path().join(".hidden"), "x").expect("written");
    fs::create_dir_all(root.path().join("nested")).expect("made");
    let dir = SecretDir::open(root.path()).expect("the directory opens");
    assert_eq!(SENTINEL, value(&dir, "hmac"));
    assert_eq!(["client.crt", "client.key", "hmac"], dir.names().as_slice());
    let pair = dir.get_pair("client").expect("the pair reads");
    assert_eq!("CERT", pair.certificate());
    assert_eq!(format!("{SENTINEL}-key"), pair.key().expose_secret());
    for shown in [
        format!("{dir:?}"),
        format!("{:?}", dir.snapshot()),
        format!("{pair:?}"),
    ] {
        assert!(!shown.contains(SENTINEL), "{shown}");
        assert!(!shown.contains("CERT"), "{shown}");
    }
}

#[test]
fn every_error_names_the_secret_and_never_its_value() {
    let root = tempfile::tempdir().expect("a directory");
    fs::write(root.path().join("empty"), "\n").expect("written");
    fs::write(root.path().join("bytes"), [0xff, 0xfe, 0x00]).expect("written");
    let locked = root.path().join("locked");
    fs::write(&locked, SENTINEL).expect("written");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).expect("locked");
    let unreadable_by_owner = fs::read(&locked).is_err();
    fs::write(root.path().join("half.crt"), SENTINEL).expect("written");
    let dir = SecretDir::open(root.path()).expect("the directory opens");

    let missing = dir.get("absent").expect_err("a missing secret is refused");
    assert!(matches!(&missing, SecretDirError::Missing { name } if name == "absent"));
    assert_eq!(
        "secret \"absent\" is not in the secret directory",
        missing.to_string()
    );
    let empty = dir.get("empty").expect_err("an empty secret is refused");
    assert!(matches!(&empty, SecretDirError::Empty { name } if name == "empty"));
    let bytes = dir.get("bytes").expect_err("bytes are refused");
    assert!(matches!(&bytes, SecretDirError::NotUtf8 { name } if name == "bytes"));
    let half = dir.get_pair("half").expect_err("a pair needs both files");
    assert!(matches!(&half, SecretDirError::Missing { name } if name == "half.key"));
    for name in ["../etc/passwd", "a/b", ".hidden", "..data", ""] {
        let invalid = dir.get(name).expect_err("the name is refused");
        assert!(
            matches!(&invalid, SecretDirError::InvalidName { .. }),
            "{invalid:?}"
        );
    }
    if unreadable_by_owner {
        let read = dir
            .get("locked")
            .expect_err("an unreadable file is refused");
        assert!(matches!(&read, SecretDirError::Read { name, .. } if name == "locked"));
        let io = read
            .source()
            .and_then(|source| source.downcast_ref::<std::io::Error>())
            .expect("the cause is the I/O error");
        assert_eq!(std::io::ErrorKind::PermissionDenied, io.kind());
    }
    for error in [missing, empty, bytes, half] {
        assert!(!error.to_string().contains(SENTINEL), "{error}");
        assert!(!format!("{error:?}").contains(SENTINEL), "{error:?}");
    }
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o600)).expect("unlocked");
}

#[test]
fn open_refuses_a_path_that_is_no_directory() {
    let root = tempfile::tempdir().expect("a directory");
    let file = root.path().join("file");
    fs::write(&file, "x").expect("written");
    let error = SecretDir::open(&file).expect_err("a file is refused");
    assert!(matches!(&error, SecretDirError::NotADirectory { path } if *path == file));
    let error = SecretDir::open(root.path().join("absent")).expect_err("a missing path is refused");
    assert!(
        matches!(&error, SecretDirError::Unreadable { .. }),
        "{error:?}"
    );
    assert!(error.source().is_some(), "the cause is carried");
}

#[test]
fn a_file_replaced_in_a_plain_directory_is_read_on_refresh() {
    let root = tempfile::tempdir().expect("a directory");
    fs::write(root.path().join("hmac"), "one").expect("written");
    let dir = SecretDir::open(root.path()).expect("the directory opens");
    let staged = root.path().join(".hmac.tmp");
    fs::write(&staged, "two").expect("written");
    fs::rename(&staged, root.path().join("hmac")).expect("renamed");
    assert_eq!(
        "one",
        value(&dir, "hmac"),
        "a read answers from the snapshot"
    );
    dir.refresh().expect("the directory reads");
    assert_eq!("two", value(&dir, "hmac"));
}

#[test]
fn a_kubernetes_swap_rotates_adds_and_removes_keys() {
    let root = tempfile::tempdir().expect("a directory");
    publish(root.path(), 1, &[("hmac", "one\n"), ("old", "gone")]);
    let dir = SecretDir::open(root.path()).expect("the directory opens");
    assert_eq!(["hmac", "old"], dir.names().as_slice());
    assert_eq!("one", value(&dir, "hmac"));

    publish(
        root.path(),
        2,
        &[("hmac", "two\n"), ("client.crt", "C"), ("client.key", "K")],
    );
    dir.refresh().expect("the directory reads");
    assert_eq!("two", value(&dir, "hmac"), "a rotated key is read");
    let pair = dir.get_pair("client").expect("a new key appears");
    assert_eq!("C", pair.certificate());
    let removed = dir.get("old").expect_err("a removed key is gone");
    assert!(
        matches!(removed, SecretDirError::Missing { .. }),
        "{removed:?}"
    );
    assert_eq!(["client.crt", "client.key", "hmac"], dir.names().as_slice());
}

#[test]
fn a_read_during_kubernetes_swaps_sees_one_update_whole() {
    let root = tempfile::tempdir().expect("a directory");
    publish_update(root.path(), 0);
    let dir = SecretDir::open(root.path()).expect("the directory opens");
    let done = Arc::new(AtomicBool::new(false));
    let writer = {
        let root = root.path().to_path_buf();
        let done = Arc::clone(&done);
        thread::spawn(move || {
            let mut generation = 1;
            while !done.load(Ordering::Relaxed) {
                publish_update(&root, generation);
                generation += 1;
            }
            generation
        })
    };
    let mut seen = std::collections::BTreeSet::new();
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        match dir.refresh() {
            Ok(()) | Err(SecretDirError::Unstable { .. }) => {}
            Err(error) => panic!("a refresh fails: {error:?}"),
        }
        let snapshot = dir.snapshot();
        assert_eq!(8, snapshot.names().count(), "{snapshot:?}");
        let values: std::collections::BTreeSet<String> = snapshot
            .names()
            .map(|name| {
                snapshot
                    .get(name)
                    .expect("a key reads")
                    .expose_secret()
                    .to_owned()
            })
            .collect();
        assert_eq!(1, values.len(), "a torn read: {values:?}");
        seen.extend(values);
    }
    done.store(true, Ordering::Relaxed);
    let published = writer.join().expect("the writer ends");
    assert!(published > 2, "the writer swapped {published} times");
    assert!(
        seen.len() > 1,
        "the reads met more than one update: {seen:?}"
    );
}

#[test]
fn concurrent_readers_during_swaps_see_consistent_snapshots() {
    let root = tempfile::tempdir().expect("a directory");
    publish_update(root.path(), 0);
    let dir = SecretDir::open(root.path()).expect("the directory opens");
    let done = Arc::new(AtomicBool::new(false));
    let reads = Arc::new(AtomicUsize::new(0));
    let readers: Vec<_> = (0..4)
        .map(|_| {
            let dir = dir.clone();
            let done = Arc::clone(&done);
            let reads = Arc::clone(&reads);
            thread::spawn(move || {
                while !done.load(Ordering::Relaxed) {
                    let snapshot = dir.snapshot();
                    let first = snapshot.get("key0").expect("key0 reads");
                    let last = snapshot.get("key7").expect("key7 reads");
                    assert_eq!(
                        first.expose_secret(),
                        last.expose_secret(),
                        "a torn snapshot"
                    );
                    reads.fetch_add(1, Ordering::Relaxed);
                }
            })
        })
        .collect();
    for generation in 1..=200 {
        publish_update(root.path(), generation);
        dir.refresh().expect("the directory reads");
    }
    done.store(true, Ordering::Relaxed);
    for reader in readers {
        reader
            .join()
            .expect("a reader saw only consistent snapshots");
    }
    assert!(reads.load(Ordering::Relaxed) > 0, "the readers read");
    assert_eq!("gen-200", value(&dir, "key3"));
}

#[test]
fn a_watch_picks_up_a_swap_and_reports_a_failed_read() {
    let parent = tempfile::tempdir().expect("a directory");
    let root = parent.path().join("secrets");
    fs::create_dir_all(&root).expect("made");
    publish(&root, 1, &[("hmac", "one")]);
    let dir = SecretDir::open(&root).expect("the directory opens");
    let failures = Arc::new(AtomicUsize::new(0));
    let watch = {
        let failures = Arc::clone(&failures);
        dir.watch(Duration::from_millis(10), move |_| {
            failures.fetch_add(1, Ordering::Relaxed);
        })
        .expect("the watch starts")
    };
    publish(&root, 2, &[("hmac", "two")]);
    let deadline = Instant::now() + Duration::from_secs(5);
    while value(&dir, "hmac") != "two" {
        assert!(Instant::now() < deadline, "the watch never read the swap");
        thread::sleep(Duration::from_millis(5));
    }
    fs::rename(&root, parent.path().join("moved")).expect("the directory moves away");
    while failures.load(Ordering::Relaxed) == 0 {
        assert!(
            Instant::now() < deadline,
            "the watch never reported the failure"
        );
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        "two",
        value(&dir, "hmac"),
        "a failed read keeps the snapshot"
    );
    drop(watch);
    let zero = dir
        .watch(Duration::ZERO, |_| {})
        .expect_err("a zero interval is refused");
    assert!(matches!(zero, SecretDirError::ZeroInterval), "{zero:?}");
}

#[test]
fn the_configuration_takes_a_directory_and_a_poll_interval() {
    let config: SecretDirConfig =
        toml::from_str("dir = \"/run/secrets/sources\"\n").expect("the configuration reads");
    assert_eq!(Path::new("/run/secrets/sources"), config.dir);
    assert_eq!(PollSeconds::default(), config.poll_seconds);
    assert_eq!(Duration::from_secs(10), config.poll_seconds.interval());
    let config: SecretDirConfig =
        toml::from_str("dir = \"/s\"\npoll_seconds = 3\n").expect("the configuration reads");
    assert_eq!(Duration::from_secs(3), config.poll_seconds.interval());
    for text in [
        "dir = \"/s\"\npoll_seconds = 0\n",
        "dir = \"/s\"\nfile = \"x\"\n",
        "",
    ] {
        let refused = toml::from_str::<SecretDirConfig>(text);
        assert!(refused.is_err(), "{text:?} is refused");
    }
}
