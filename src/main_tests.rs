use clap::{CommandFactory, Parser};

use super::{Cli, Command};

#[test]
fn the_command_line_definition_is_consistent() {
    // Conflicting ids, flags on positionals, defaults outside value parsers…
    Cli::command().debug_assert();
}

#[test]
fn a_search_gets_its_stations_options_and_endpoints() {
    let cli = Cli::try_parse_from([
        "umstieg",
        "Bad Dürkheim Hbf",
        "Göteborg Centralstation",
        "--window",
        "30",
        "--planned",
        "--via",
        "Hamburg Hbf",
        "--via",
        "København H",
        "--api",
        "http://motis.test",
        "--db-api",
        "http://db.test",
    ])
    .unwrap();
    assert!(cli.command.is_none());
    // A nested argument group once made this `None` although FROM and TO were given.
    let search = cli.search.expect("FROM and TO given");
    assert_eq!((search.from.as_str(), search.to.as_str()), ("Bad Dürkheim Hbf", "Göteborg Centralstation"));
    assert_eq!(search.window, 30);
    assert!(search.planned);
    assert_eq!(search.via, ["Hamburg Hbf", "København H"]);
    assert_eq!(cli.endpoints.api, "http://motis.test");
    assert_eq!(cli.endpoints.db_api, "http://db.test");
}

#[test]
fn a_search_needs_both_stations() {
    assert!(Cli::try_parse_from(["umstieg", "Mannheim Hbf"]).is_err());
    assert!(Cli::try_parse_from(["umstieg"]).is_err());
}

#[test]
fn serve_takes_its_own_endpoints_and_no_stations() {
    let cli =
        Cli::try_parse_from(["umstieg", "serve", "--addr", "127.0.0.1:8765", "--parallel", "2", "--efa", ""]).unwrap();
    let Some(Command::Serve { addr, parallel, endpoints }) = cli.command else { panic!("serve expected") };
    assert_eq!(addr.port(), 8765);
    assert_eq!(parallel, 2);
    assert_eq!(endpoints.efa, "", "an empty EFA URL switches EFA off");
    assert!(cli.search.is_none());
}

#[test]
fn search_options_are_rejected_with_serve() {
    assert!(Cli::try_parse_from(["umstieg", "serve", "--window", "30"]).is_err());
    assert!(Cli::try_parse_from(["umstieg", "A", "B", "serve"]).is_err());
}

#[test]
fn out_of_range_options_are_rejected_instead_of_overflowing() {
    let cases = [
        ("--window", "99999999999999"),
        ("--window", "-1"),
        ("--window", "10081"),
        ("--extra-hours", "1e300"),
        ("--extra-hours", "NaN"),
        ("--extra-hours", "inf"),
        ("--extra-hours", "-1"),
        ("--min-transfer", "99999999999999"),
        ("--min-transfer", "-5"),
        ("--missed-by", "99999999999999"),
        ("--max-changes", "101"),
        ("--max-nodes", "-1"),
    ];
    for (option, value) in cases {
        let Err(error) = Cli::try_parse_from(["umstieg", "A", "B", &format!("{option}={value}")]) else {
            panic!("{option} {value} accepted");
        };
        assert_eq!(error.kind(), clap::error::ErrorKind::ValueValidation, "{option} {value}: {error}");
    }
    let extremes = ["--window=10080", "--extra-hours=168", "--min-transfer=0", "--missed-by=0", "--max-changes=100"];
    let search = Cli::try_parse_from(["umstieg", "A", "B"].into_iter().chain(extremes)).unwrap().search.unwrap();
    assert_eq!((search.window, search.extra_hours, search.max_changes), (10080, 168.0, 100));
}
