# Everything CI-worthy: formatting, all lints as errors, tests, docs, and the
# web pages (TypeScript strict + ESLint strict, bundled output up to date).
check: web-check
    cargo fmt --check
    cargo clippy --all-targets --locked -- -D warnings
    cargo test --locked
    RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --document-private-items --locked

# Type-check, lint and format-check the TypeScript; the committed web/dist
# (what the binary embeds) must match a fresh bundle.
web-check:
    #!/usr/bin/env bash
    set -euo pipefail
    cd web
    npm run --silent check
    fresh=$(mktemp -d)
    trap 'rm -rf "$fresh"' EXIT
    bun build plot.ts form.ts progress.ts --outdir "$fresh" --target browser --format esm > /dev/null
    diff -r "$fresh" dist || { echo "web/dist is stale: run 'just web'"; exit 1; }
    if grep -qi '</script' dist/*.js; then echo "a script contains </script"; exit 1; fi

# Bundle web/*.ts into web/dist (committed: building umstieg needs no Node or Bun).
web:
    cd web && npm run --silent build

fmt:
    cargo fmt
    cd web && npx prettier --write .
