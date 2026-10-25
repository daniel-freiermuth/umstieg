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

# Mutation testing: a missed mutant fails; exit code 3 (only timeouts, i.e. the
# mutant made a test hang) counts as caught. All Rust mutants (about an hour):
mutants:
    cargo mutants --no-shuffle || [ $? -eq 3 ]

# Only the Rust mutants in lines changed since `base` (what CI gates PRs on).
mutants-diff base="origin/main":
    mkdir -p target && git diff "{{base}}..." > target/mutants.diff
    cargo mutants --no-shuffle --in-diff target/mutants.diff || [ $? -eq 3 ]

# All mutants of the web helper modules (the DOM scripts aren't mutated).
web-mutants:
    cd web && npx stryker run

# Only the web helper mutants in lines changed since `base` (what CI gates PRs on).
web-mutants-diff base="origin/main":
    #!/usr/bin/env bash
    set -euo pipefail
    cd web
    read -ra helpers <<< "$(node -p "require('./stryker.config.json').mutate.join(' ')")"
    # Changed line ranges of the helpers, as Stryker's `file.ts:first-last`.
    ranges=$(git diff -U0 --no-color "{{base}}..." -- "${helpers[@]}" | awk '
        /^\+\+\+ / { file = substr($2, 7); next }  # "b/web/reach.ts" -> "reach.ts"
        /^@@ / {
            split(substr($3, 2), a, ",")
            n = (a[2] == "") ? 1 : a[2]
            if (n > 0) { printf "%s%s:%d-%d", sep, file, a[1], a[1] + n - 1; sep = "," }
        }')
    if [ -z "$ranges" ]; then echo "no web helper lines changed"; exit 0; fi
    echo "mutating $ranges"
    npx stryker run --mutate "$ranges"

fmt:
    cargo fmt
    cd web && npx prettier --write .
