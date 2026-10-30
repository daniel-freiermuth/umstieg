use super::Keys;
use crate::testkit;

const NO_ENV: fn(&str) -> Option<String> = |_| None;

/// DB's credentials as "client/key", or "-".
fn db(keys: &Keys) -> String {
    keys.db.as_ref().map_or_else(|| "-".to_owned(), |c| format!("{}/{}", c.client_id, c.api_key))
}

#[test]
fn keys_from_the_file_and_the_environment() {
    let file = "# DB API\nexport DB_CLIENT_ID=\"abc\"\n  DB_API_KEY = k=ey  \n\nTRAFIKVERKET_KEY=\"t-key\"\n";
    let all = Keys::parse(file, NO_ENV);
    assert_eq!((db(&all), all.trafikverket.as_deref()), ("abc/k=ey".to_owned(), Some("t-key")));

    let env = |key: &str| (key == "DB_API_KEY" || key == "TRAFIKVERKET_KEY").then(|| "from-env".to_owned());
    let mixed = Keys::parse(file, env);
    assert_eq!((db(&mixed), mixed.trafikverket.as_deref()), ("abc/from-env".to_owned(), Some("from-env")));

    let half = Keys::parse("DB_CLIENT_ID=abc\n", NO_ENV);
    assert_eq!((db(&half), half.trafikverket), ("-".to_owned(), None), "DB needs both values");
    let only_env = Keys::parse("", |key: &str| Some(format!("{key}!")));
    assert_eq!(db(&only_env), "DB_CLIENT_ID!/DB_API_KEY!");
    assert_eq!(only_env.trafikverket.as_deref(), Some("TRAFIKVERKET_KEY!"));
}

/// `Keys::load` as `"client/key trafikverket"` ("-" for missing), or `"error"`.
fn loaded() -> String {
    match Keys::load() {
        Ok(keys) => format!("{} {}", db(&keys), keys.trafikverket.as_deref().unwrap_or("-")),
        Err(_) => "error".to_owned(),
    }
}

#[test]
fn keys_load_from_the_config_directory_or_the_environment() {
    const EXPECT: &str = "UMSTIEG_TEST_EXPECT";
    if testkit::in_child() {
        assert_eq!(loaded(), std::env::var(EXPECT).unwrap());
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config");
    std::fs::create_dir_all(config.join("umstieg")).unwrap();
    std::fs::write(config.join("umstieg/keys.env"), "DB_CLIENT_ID=abc\nDB_API_KEY=key\nTRAFIKVERKET_KEY=tv\n").unwrap();
    let home = dir.path().join("home");
    std::fs::create_dir_all(home.join(".config/umstieg")).unwrap();
    std::fs::write(home.join(".config/umstieg/keys.env"), "DB_CLIENT_ID=home\nDB_API_KEY=key\n").unwrap();
    let empty = dir.path().join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    let unreadable = dir.path().join("unreadable");
    std::fs::create_dir_all(unreadable.join("umstieg/keys.env")).unwrap();

    let test = "keys::tests::keys_load_from_the_config_directory_or_the_environment";
    let run = |vars: &[(&str, &str)], expect: &str| {
        let mut env: Vec<(&str, &std::ffi::OsStr)> = vec![(EXPECT, expect.as_ref()), ("HOME", home.as_os_str())];
        env.extend(vars.iter().map(|&(k, v)| (k, v.as_ref())));
        testkit::rerun_with_env(test, &env);
    };
    let (config, empty, unreadable) = (config.to_str().unwrap(), empty.to_str().unwrap(), unreadable.to_str().unwrap());
    run(&[("XDG_CONFIG_HOME", config)], "abc/key tv");
    run(&[], "home/key -");
    run(&[("XDG_CONFIG_HOME", empty)], "- -");
    run(&[("XDG_CONFIG_HOME", empty), ("DB_CLIENT_ID", "id"), ("DB_API_KEY", "env")], "id/env -");
    run(&[("XDG_CONFIG_HOME", config), ("DB_API_KEY", "env"), ("TRAFIKVERKET_KEY", "tv-env")], "abc/env tv-env");
    run(&[("XDG_CONFIG_HOME", unreadable)], "error");
    run(&[("XDG_CONFIG_HOME", "")], "home/key -");
    run(&[("XDG_CONFIG_HOME", "config")], "home/key -");
}
