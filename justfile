# Everything CI-worthy: formatting, all lints as errors, tests, docs.
check:
    cargo fmt --check
    cargo clippy --all-targets --locked -- -D warnings
    cargo test --locked
    RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --document-private-items --locked

fmt:
    cargo fmt
