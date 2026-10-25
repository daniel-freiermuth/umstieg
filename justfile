# Everything CI runs (.github/workflows/ci.yml runs these recipes one per job):
# formatting, all lints as errors, tests, docs, and the web pages.
check: fmt-check clippy test doc web-check

fmt-check:
    cargo fmt --check

clippy:
    cargo clippy --all-targets --locked -- -D warnings

test:
    cargo nextest run --locked

doc:
    RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --document-private-items --locked

# Type-check, lint, format-check and test the TypeScript; the committed web/dist
# (what the binary embeds) must match a fresh bundle.
web-check:
    #!/usr/bin/env bash
    set -euo pipefail
    cd web
    npm run --silent check
    bun test
    fresh=$(mktemp -d)
    trap 'rm -rf "$fresh"' EXIT
    bun build plot.ts form.ts progress.ts --outdir "$fresh" --target browser --format esm > /dev/null
    diff -r "$fresh" dist || { echo "web/dist is stale: run 'just web'"; exit 1; }
    if grep -qi '</script' dist/*.js; then echo "a script contains </script"; exit 1; fi

# Bundle web/*.ts into web/dist (committed: building umstieg needs no Node or Bun).
web:
    cd web && npm run --silent build

# Line coverage of the Rust code (HTML in target/llvm-cov/html) and of the web modules.
coverage:
    cargo llvm-cov nextest --locked --html
    cargo llvm-cov report --summary-only
    cd web && bun test --coverage

fmt:
    cargo fmt
    cd web && npx prettier --write .
