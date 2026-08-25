#!/usr/bin/env bash
#
# Rejects every dependency edge that `docs/architecture.md` does not permit.
#
# The script reads the layer table of that document, so the policy and the
# architecture can never disagree. It then reads `cargo tree` for each crate of
# the workspace and compares the two.
#
# The script checks four rules:
#
#   1. A direct kvim edge must appear in the table row of its crate.
#   2. A direct kvim edge must reach a lower layer.
#   3. A transitive kvim edge must stay inside the closure of the table row.
#   4. An isolation crate must reach none of the external crates that its
#      charter refuses.
#
# It also proves that every dependency of a supported external package is
# available to a consumer: one path below `crates/`, or one registry release.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
readonly REPO_ROOT
readonly ARCHITECTURE="$REPO_ROOT/docs/architecture.md"

# The supported external packages of `docs/architecture.md`.
readonly PUBLIC_PACKAGES="kvim-path kvim-core kvim-settings kvim-keymap kvim-input kvim-editor kvim-syntax kvim-lsp kvim-ui kvim-tui"

# The external crates that one isolation charter refuses.
#
# `docs/architecture.md` states that no syntax-only consumer compiles LSP,
# ratatui, or the editor, and that `kvim-keymap`, `kvim-path`, and `kvim-syntax`
# are layer 0 charters. `docs/embedding.md` keeps `kvim-ui` a pure geometry and
# composition charter. Each row below names one crate and the external crates
# that its tree must never hold.
readonly FORBIDDEN_EXTERNAL="\
kvim-syntax crossterm notify ratatui tokio tokio-util
kvim-keymap cap-std crossterm notify ratatui tokio tokio-util
kvim-path crossterm notify ratatui tokio tokio-util
kvim-lsp crossterm ratatui
kvim-input cap-std crossterm notify ratatui tokio tokio-util
kvim-editor cap-std crossterm notify ratatui tokio tokio-util
kvim-ui cap-std notify tokio tokio-util"

WORK="$(mktemp -d)"
readonly WORK
trap 'rm -rf "$WORK"' EXIT

readonly TABLE="$WORK/table"
readonly CLOSURE="$WORK/closure"

failures=0

fail() {
    printf 'dependency policy: %s\n' "$1" >&2
    failures=$((failures + 1))
}

# Reads the layer table of the architecture document.
#
# Each output line is `<layer> <crate> <allowed direct kvim dependencies>`. A
# cell that says `every library above` becomes the marker `__ALL_BELOW__`, which
# the caller expands against the layer numbers.
parse_layer_table() {
    awk '
        /^## Dependency Direction/ { inside = 1; next }
        inside && /^## / { inside = 0 }
        inside && /^\|[[:space:]]*[0-9]+[[:space:]]*\|/ {
            split($0, cell, "|")
            layer = cell[2]
            name = cell[3]
            deps = cell[4]
            gsub(/[`[:space:]]/, "", layer)
            gsub(/[`[:space:]]/, "", name)
            allowed = ""
            rest = deps
            while (match(rest, /`kvim-[a-z-]+`/)) {
                allowed = allowed " " substr(rest, RSTART + 1, RLENGTH - 2)
                rest = substr(rest, RSTART + RLENGTH)
            }
            if (deps ~ /every library above/) {
                allowed = allowed " __ALL_BELOW__"
            }
            print layer, name, allowed
        }
    ' "$ARCHITECTURE"
}

# Replaces the `__ALL_BELOW__` marker with every crate of a lower layer.
expand_all_below() {
    local layer name allowed expanded other_layer other_name
    while read -r layer name allowed; do
        if [[ "$allowed" != *__ALL_BELOW__* ]]; then
            printf '%s %s %s\n' "$layer" "$name" "$allowed"
            continue
        fi
        expanded="${allowed//__ALL_BELOW__/}"
        while read -r other_layer other_name _; do
            if [[ "$other_layer" -lt "$layer" ]]; then
                expanded="$expanded $other_name"
            fi
        done <"$TABLE.raw"
        printf '%s %s %s\n' "$layer" "$name" "$expanded"
    done <"$TABLE.raw"
}

# Returns the layer of one crate.
layer_of() {
    awk -v name="$1" '$2 == name { print $1 }' "$TABLE"
}

# Builds the transitive closure of the permitted kvim edges.
#
# The table is one directed acyclic graph of at most one row for each crate, so
# the fixed point arrives after fewer rounds than there are crates.
#
# The copy below is written once and then only read, so the read and the write
# never overlap.
# shellcheck disable=SC2094
build_closure() {
    local rounds=0 changed=1 name allowed dependency reached extra
    cut -d' ' -f2- "$TABLE" >"$CLOSURE"
    while [[ "$changed" -eq 1 ]]; do
        changed=0
        rounds=$((rounds + 1))
        if [[ "$rounds" -gt 32 ]]; then
            fail "the permitted layer table did not settle, so it holds a cycle"
            return
        fi
        : >"$CLOSURE.next"
        # The round reads one snapshot and writes one new file, so no lookup
        # sees a half-written round.
        cp "$CLOSURE" "$CLOSURE.round"
        while read -r name allowed; do
            reached="$allowed"
            for dependency in $allowed; do
                extra="$(awk -v name="$dependency" '$1 == name { $1 = ""; print }' "$CLOSURE.round")"
                reached="$reached $extra"
            done
            reached="$(printf '%s' "$reached" | tr ' ' '\n' | sed '/^$/d' | sort -u | tr '\n' ' ')"
            printf '%s %s\n' "$name" "$reached" >>"$CLOSURE.next"
        done <"$CLOSURE.round"
        if ! cmp -s "$CLOSURE" "$CLOSURE.next"; then
            changed=1
        fi
        mv "$CLOSURE.next" "$CLOSURE"
    done
}

# Prints the kvim crates that one dependency tree holds, without its own name.
kvim_edges() {
    local name="$1"
    shift
    cargo tree --quiet -p "$name" -e normal --all-features --prefix none --no-dedupe "$@" |
        awk '{ print $1 }' |
        grep '^kvim-' |
        grep -v "^$name\$" |
        sort -u
}

check_edges() {
    local layer name allowed permitted dependency dependency_layer
    while read -r layer name allowed; do
        for dependency in $(kvim_edges "$name" --depth 1); do
            if [[ " $allowed " != *" $dependency "* ]]; then
                fail "$name depends on $dependency, which its architecture row refuses"
            fi
            dependency_layer="$(layer_of "$dependency")"
            if [[ -n "$dependency_layer" && "$dependency_layer" -ge "$layer" ]]; then
                fail "$name is layer $layer and depends on $dependency of layer $dependency_layer"
            fi
        done

        permitted="$(awk -v name="$name" '$1 == name { $1 = ""; print }' "$CLOSURE")"
        for dependency in $(kvim_edges "$name"); do
            if [[ " $permitted " != *" $dependency "* ]]; then
                fail "$name reaches $dependency, which no permitted edge explains"
            fi
        done
    done <"$TABLE"
}

check_external_isolation() {
    local name forbidden reached crate
    while read -r name forbidden; do
        [[ -n "$name" ]] || continue
        reached="$(cargo tree --quiet -p "$name" -e normal --all-features --prefix none --no-dedupe |
            awk '{ print $1 }' | sort -u)"
        for crate in $forbidden; do
            if printf '%s\n' "$reached" | grep -qx "$crate"; then
                fail "$name reaches $crate, but its charter compiles no $crate"
            fi
        done
    done <<<"$FORBIDDEN_EXTERNAL"
}

check_public_packages() {
    local name line path
    for name in $PUBLIC_PACKAGES; do
        while read -r line; do
            path="${line#*(}"
            path="${path%)*}"
            if [[ "$path" != "$REPO_ROOT/crates/"* ]]; then
                fail "$name depends on $path, which no consumer of this repository can reach"
            fi
        done < <(cargo tree --quiet -p "$name" -e normal --all-features --prefix none --no-dedupe |
            grep '(/' || true)

        while read -r line; do
            fail "$name enables a test seam in a normal build: $line"
        done < <(cargo tree --quiet -p "$name" -e normal --prefix none --no-dedupe -f '{p} {f}' |
            grep 'test-support' || true)
    done
}

usage() {
    cat <<'USAGE'
Usage: check-dependency-edges.sh
       check-dependency-edges.sh --reject <crate> <forbidden crate>...

Without arguments the script checks every rule of docs/architecture.md.

With `--reject` it checks one named rule: the dependency tree of <crate> must
hold none of the crates that follow. Use it for a rule that one job must report
under its own name.
USAGE
}

reject_named_edges() {
    local name="$1"
    shift
    local reached crate
    reached="$(cargo tree --quiet -p "$name" -e normal --all-features --prefix none --no-dedupe |
        awk '{ print $1 }' | sort -u)"
    if [[ -z "$reached" ]]; then
        fail "cargo tree returned nothing for $name"
        return
    fi
    for crate in "$@"; do
        if printf '%s\n' "$reached" | grep -qx "$crate"; then
            fail "$name reaches $crate, which its charter refuses"
        fi
    done
}

main() {
    cd "$REPO_ROOT"

    if [[ "${1:-}" == "--help" || "${1:-}" == "-h" ]]; then
        usage
        exit 0
    fi

    if [[ "${1:-}" == "--reject" ]]; then
        shift
        if [[ "$#" -lt 2 ]]; then
            usage >&2
            exit 2
        fi
        local subject="$1"
        shift
        reject_named_edges "$subject" "$@"
        if [[ "$failures" -gt 0 ]]; then
            exit 1
        fi
        printf '%s reaches none of the refused crates.\n' "$subject"
        exit 0
    fi

    parse_layer_table >"$TABLE.raw"
    if [[ ! -s "$TABLE.raw" ]]; then
        fail "docs/architecture.md holds no readable layer table"
        exit 1
    fi
    expand_all_below >"$TABLE"
    build_closure
    check_edges
    check_external_isolation
    check_public_packages

    if [[ "$failures" -gt 0 ]]; then
        printf '%s forbidden dependency edge(s) found.\n' "$failures" >&2
        exit 1
    fi
    printf 'Every dependency edge matches docs/architecture.md.\n'
}

main "$@"
