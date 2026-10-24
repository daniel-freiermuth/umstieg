# Everything CI-worthy: formatting, all lints as errors, tests, docs, and the
# web pages (TypeScript strict + ESLint strict, compiled output up to date).
check: web-check
    cargo fmt --check
    cargo clippy --all-targets --locked -- -D warnings
    cargo test --locked
    RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --document-private-items --locked

# Type-check, lint and format-check the TypeScript; the committed web/dist
# (what the binary embeds) must match a fresh build.
web-check:
    #!/usr/bin/env bash
    set -euo pipefail
    cd web
    npx tsc -p . --noEmit
    npx eslint --max-warnings 0 .
    npx prettier --check .
    fresh=$(mktemp -d)
    trap 'rm -rf "$fresh"' EXIT
    npx tsc -p . --outDir "$fresh"
    diff -r "$fresh" dist || { echo "web/dist is stale: run 'just web'"; exit 1; }
    if grep -qi '</script' dist/*.js; then echo "a script contains </script"; exit 1; fi

# Compile web/*.ts to web/dist (committed: building umstieg needs no Node).
web:
    cd web && npx tsc -p .

fmt:
    cargo fmt
    cd web && npx prettier --write .
