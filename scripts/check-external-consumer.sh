#!/usr/bin/env bash
#
# Compiles the external consumer fixture against this commit.
#
# The fixture in `fixtures/consumer` is its own workspace root, so the check
# proves that an outside repository needs no shared parent workspace. The script
# copies the fixture, points its Git dependency at this repository, pins the
# revision under test, and then compiles every combination of the public feature
# matrix of `docs/architecture.md`.
#
# The `file://` URL keeps the check offline for the kvim packages themselves. It
# reads the committed revision, so a working-tree change reaches the fixture only
# after a commit.
#
# Usage: check-external-consumer.sh [revision]

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
readonly REPO_ROOT
readonly FIXTURE="$REPO_ROOT/fixtures/consumer"

# The copy of the fixture. The trap below removes it, and a trap runs after the
# calling function has left its scope, so the value cannot be local.
work=""
trap 'rm -rf "$work"' EXIT

usage() {
    cat <<'USAGE'
Usage: check-external-consumer.sh [revision]

Compiles fixtures/consumer against one revision of this repository through a
revision-pinned Git dependency, once for every combination of the public
feature matrix. The revision defaults to HEAD.
USAGE
}

main() {
    if [[ "${1:-}" == "--help" || "${1:-}" == "-h" ]]; then
        usage
        exit 0
    fi

    local revision="${1:-HEAD}"
    revision="$(git -C "$REPO_ROOT" rev-parse "$revision")"
    local url="file://$REPO_ROOT"

    work="$(mktemp -d)"

    cp -R "$FIXTURE/." "$work/"
    sed -i.bak \
        -e "s|git = \"[^\"]*\"|git = \"$url\"|g" \
        -e "s|rev = \"[^\"]*\"|rev = \"$revision\"|g" \
        "$work/Cargo.toml"
    rm -f "$work/Cargo.toml.bak"

    printf 'Consumer fixture pins %s at %s\n' "$url" "$revision"

    local grammars
    grammars="$(grep -oE '^grammar-[a-z]+' "$work/Cargo.toml" | sort -u)"
    if [[ -z "$grammars" ]]; then
        printf 'the fixture declares no grammar feature\n' >&2
        exit 1
    fi

    # The default build bundles no grammar, which is the first required
    # combination of every public crate.
    run_check "$work" 'no grammar'

    local grammar
    for grammar in $grammars; do
        run_check "$work" "$grammar" --features "$grammar"
    done

    run_check "$work" 'all-grammars' --features all-grammars

    # One run proves that the consumer also links and answers, not only that it
    # type-checks.
    cargo run --quiet --manifest-path "$work/Cargo.toml"

    printf 'The external consumer compiles every combination of the public feature matrix.\n'
}

run_check() {
    local directory="$1" label="$2"
    shift 2
    printf '  checking %s\n' "$label"
    cargo check --quiet --manifest-path "$directory/Cargo.toml" "$@"
}

main "$@"
