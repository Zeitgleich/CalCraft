use super::*;

#[test]
fn slugs_are_derived_safely() {
    let slug = |name| derive_slug(name, CollectionKind::Calendar).to_string();
    assert_eq!(slug("work"), "work");
    assert_eq!(slug("Tasks List"), "tasks-list");
    assert_eq!(slug("Büro & Co"), "b-ro-co");
    assert_eq!(slug("_hidden"), "hidden");
    assert_eq!(
        slug("a1b2c3d4-0000-4000-8000-000000000000"),
        "a1b2c3d4-0000-4000-8000-000000000000"
    );
    assert_eq!(slug("../.."), "calendar");
    assert_eq!(slug(""), "calendar");
    assert_eq!(
        derive_slug("☃", CollectionKind::Addressbook).as_str(),
        "addressbook"
    );
    assert_eq!(slug(&"x".repeat(200)).len(), Slug::MAX_LEN);
}

#[test]
fn slugs_are_unique_per_kind() {
    let mut used = HashSet::new();
    let work: Slug = "work".parse().unwrap();
    assert_eq!(
        unique_slug(work.clone(), CollectionKind::Calendar, &mut used).as_str(),
        "work"
    );
    assert_eq!(
        unique_slug(work.clone(), CollectionKind::Calendar, &mut used).as_str(),
        "work-2"
    );
    assert_eq!(
        unique_slug(work.clone(), CollectionKind::Calendar, &mut used).as_str(),
        "work-3"
    );
    assert_eq!(
        unique_slug(work, CollectionKind::Addressbook, &mut used).as_str(),
        "work"
    );
    let long: Slug = "y".repeat(Slug::MAX_LEN).parse().unwrap();
    unique_slug(long.clone(), CollectionKind::Calendar, &mut used);
    let next = unique_slug(long, CollectionKind::Calendar, &mut used);
    assert!(next.as_str().ends_with("-2") && next.as_str().len() == Slug::MAX_LEN);
}

#[test]
fn map_entries_are_validated() {
    let map = parse_map(&["home = personal".to_owned()]).unwrap();
    assert_eq!(map[0].0, "home");
    assert_eq!(map[0].1.as_str(), "personal");
    assert!(parse_map(&["home".to_owned()]).is_err());
    assert!(parse_map(&["home=Not Valid".to_owned()]).is_err());
}

#[test]
fn foreign_owners_are_detected() {
    let me = "/remote.php/dav/principals/users/alice";
    assert!(!foreign_owner("principals/users/alice", me));
    assert!(!foreign_owner(
        "/remote.php/dav/principals/users/alice/",
        me
    ));
    assert!(!foreign_owner(
        "https://cloud.example.com/remote.php/dav/principals/users/alice/",
        me
    ));
    assert!(foreign_owner("principals/users/bob", me));
    assert!(foreign_owner("/remote.php/dav/principals/users/bob/", me));
    assert!(!foreign_owner("", me));
    // Whole segments: `ice` is not `alice`, `users/alice` is not `xusers/alice`.
    assert!(foreign_owner("ice", me));
    assert!(foreign_owner("xusers/alice", me));
    assert!(foreign_owner("/principals/users/alice/extra/", me));
}

#[test]
fn line_endings_are_restored() {
    assert_eq!(restore_line_endings("A\nB\n"), b"A\r\nB\r\n");
    assert_eq!(restore_line_endings("\n  A\r\nB\r\n  "), b"A\r\nB\r\n");
}

#[test]
fn object_names_are_decoded() {
    assert_eq!(object_name("/dav/cal/a%40b.ics"), "a@b.ics");
}

#[test]
fn urls_must_be_https_without_credentials() {
    assert!(parse_url("https://nas.example.com:5001/caldav/", false).is_ok());
    assert!(parse_url("http://nas.example.com:5000/caldav/", false).is_err());
    assert!(parse_url("http://nas.example.com:5000/caldav/", true).is_ok());
    assert!(parse_url("https://user:pw@nas.example.com/", false).is_err());
    assert!(parse_url("ftp://nas.example.com/", true).is_err());
    assert!(parse_url("/caldav/", true).is_err());
}

#[cfg(unix)]
#[test]
fn password_files_must_be_private() {
    use std::os::unix::fs::PermissionsExt;

    let dir = std::env::temp_dir().join(format!("calcraft-pw-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("password");
    std::fs::write(&path, "s3cret pass\nignored\n").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    let err = read_password_file(&path).unwrap_err().to_string();
    assert!(err.contains("chmod 600"), "{err}");
    assert!(!err.contains("s3cret"));
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(read_password_file(&path).unwrap().expose(), "s3cret pass");
    std::fs::write(&path, "\n").unwrap();
    assert!(read_password_file(&path).is_err());

    // Links are not followed, directories are no password files.
    std::fs::write(&path, "s3cret pass\n").unwrap();
    let link = dir.join("link");
    std::os::unix::fs::symlink(&path, &link).unwrap();
    let err = read_password_file(&link).unwrap_err().to_string();
    assert!(err.contains("regular file"), "{err}");
    assert!(read_password_file(&dir).is_err());
    std::fs::remove_dir_all(&dir).unwrap();
}
