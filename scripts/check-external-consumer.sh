#!/usr/bin/env bash
# Build each supported package as an independent outside-workspace consumer.

set -euo pipefail
IFS=$'\n\t'

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
readonly REPO_ROOT
readonly FIXTURES_ROOT="$REPO_ROOT/fixtures/consumer"
readonly CONSUMERS=(
    kvim-path kvim-fuzzy kvim-core kvim-settings kvim-keymap kvim-input
    kvim-editor kvim-syntax kvim-lsp kvim-ui kvim-embed-memory
    kvim-embed-worktree
)

work=""
source_copy=""
SOURCE_URL=""
SOURCE_REVISION=""
trap 'rm -rf "$work" "$source_copy"' EXIT

usage() {
    cat <<'USAGE'
Usage: check-external-consumer.sh [--local-source] [--checked-out-repository]
                                  [--repository-url URL] [revision]

Build each supported package from an independent Cargo workspace. Remote mode
uses the checked-out repository's origin unless --repository-url is supplied.
It pins revision, or HEAD when omitted. The script never prints the repository
URL. HTTP URLs with user information, queries, or fragments are refused.

Use --checked-out-repository to test the selected revision through a file URL
to this checkout. Use --local-source to include uncommitted Cargo.toml,
Cargo.lock, and crates/ changes in a temporary local Git repository. Local
source mode needs no remote access.
USAGE
}

is_secret_path() {
    local path="$1"
    local component
    while IFS= read -r component; do
        case "${component,,}" in
            .env|.env.*|*.pem|*.key|id_rsa|id_ed25519|*.ppk|*.p12|*.pfx|*.age|*.enc|.netrc|.npmrc|.pypirc|.git-credentials|*secret*|*credential*|*password*|*token*)
                return 0
                ;;
        esac
    done < <(printf '%s' "$path" | tr '/' '\n')
    return 1
}

is_allowed_source_path() {
    case "$1" in
        Cargo.toml|Cargo.lock|crates/*) return 0 ;;
        *) return 1 ;;
    esac
}

validate_changed_paths() {
    local path
    while IFS= read -r -d '' path; do
        if is_secret_path "$path"; then
            printf 'local source mode refuses a changed secret-bearing path\n' >&2
            exit 1
        fi
    done < <(
        git -C "$REPO_ROOT" diff --name-only --no-renames -z HEAD --
        git -C "$REPO_ROOT" ls-files --others --exclude-standard -z
    )

    while IFS= read -r -d '' path; do
        if is_secret_path "$path"; then
            printf 'local source mode refuses a tracked secret-bearing source path\n' >&2
            exit 1
        fi
    done < <(git -C "$REPO_ROOT" ls-tree -r --name-only -z HEAD -- Cargo.toml Cargo.lock crates)
}

prepare_local_source() {
    validate_changed_paths
    source_copy="$(mktemp -d)"
    git -C "$REPO_ROOT" archive HEAD -- Cargo.toml Cargo.lock crates | tar -x -C "$source_copy"

    # Apply tracked modifications, deletions, and both sides of renames exactly.
    git -C "$REPO_ROOT" diff --binary --no-renames HEAD -- Cargo.toml Cargo.lock crates \
        | git -C "$source_copy" apply --binary --whitespace=nowarn

    local path
    while IFS= read -r -d '' path; do
        is_allowed_source_path "$path" || continue
        if is_secret_path "$path"; then
            printf 'local source mode refuses an untracked secret-bearing path\n' >&2
            exit 1
        fi
        mkdir -p "$source_copy/$(dirname "$path")"
        cp -P "$REPO_ROOT/$path" "$source_copy/$path"
    done < <(git -C "$REPO_ROOT" ls-files --others --exclude-standard -z -- Cargo.toml Cargo.lock crates)

    git -C "$source_copy" init --quiet
    git -C "$source_copy" add --all
    git -C "$source_copy" \
        -c user.name=kvim-consumer-check \
        -c user.email=kvim-consumer-check.invalid \
        commit --quiet -m "Build local consumer source"

    SOURCE_URL="file://$source_copy"
    SOURCE_REVISION="$(git -C "$source_copy" rev-parse HEAD)"
}

validate_repository_url() {
    local url="$1"
    if [[ -z "$url" ]]; then
        printf 'the repository has no origin; pass --repository-url\n' >&2
        exit 2
    fi
    case "$url" in
        http://*|https://*)
            if [[ "$url" == *\?* || "$url" == *\#* || "$url" =~ ^https?://[^/@]+@ ]]; then
                printf 'refusing an HTTP repository URL that may contain credentials\n' >&2
                exit 2
            fi
            ;;
    esac
}

rewrite_dependencies() {
    local manifest="$1"
    python3 - "$manifest" "$SOURCE_URL" "$SOURCE_REVISION" <<'PY'
from pathlib import Path
import re
import sys

manifest = Path(sys.argv[1])
url, revision = sys.argv[2:]
text = manifest.read_text()
pattern = re.compile(
    r'(?m)^(kvim-[a-z0-9-]+\s*=\s*\{)([^\n}]*)(\}\s*)$'
)
expected = len(pattern.findall(text))
if expected == 0:
    raise SystemExit(f"{manifest}: no kvim Git dependencies")

def rewrite(match: re.Match[str]) -> str:
    body = match.group(2)
    git_count = len(re.findall(r'\bgit\s*=\s*"[^"]*"', body))
    rev_count = len(re.findall(r'\brev\s*=\s*"[^"]*"', body))
    if (git_count, rev_count) != (1, 1):
        raise SystemExit(
            f"{manifest}: {match.group(1).split('=')[0].strip()} needs one git and one rev"
        )
    body = re.sub(r'\bgit\s*=\s*"[^"]*"', f'git = "{url}"', body)
    body = re.sub(r'\brev\s*=\s*"[^"]*"', f'rev = "{revision}"', body)
    return match.group(1) + body + match.group(3)

rewritten, count = pattern.subn(rewrite, text)
if count != expected:
    raise SystemExit(f"{manifest}: expected {expected} rewrites, completed {count}")
for line in rewritten.splitlines():
    if re.match(r'^kvim-[a-z0-9-]+\s*=', line):
        if f'git = "{url}"' not in line or f'rev = "{revision}"' not in line:
            raise SystemExit(f"{manifest}: one kvim dependency was not fully pinned")
manifest.write_text(rewritten)
PY
}

run_consumer() {
    local name="$1"
    shift
    local label="default"
    if [[ $# -gt 0 ]]; then
        label="$1 ${2:-}"
    fi
    printf '  %-23s %s\n' "$name" "$label"
    cargo run --quiet --manifest-path "$work/$name/Cargo.toml" "$@"
}

main() {
    local mode="remote"
    local revision="HEAD"
    local repository_url=""

    while [[ $# -gt 0 ]]; do
        case "$1" in
            --local-source) mode="local"; shift ;;
            --checked-out-repository) mode="checkout"; shift ;;
            --repository-url)
                [[ $# -ge 2 ]] || { printf '%s requires a value\n' "$1" >&2; exit 2; }
                repository_url="$2"
                shift 2
                ;;
            --help|-h) usage; return ;;
            --*) printf 'unknown option: %s\n' "$1" >&2; usage >&2; exit 2 ;;
            *)
                revision="$1"
                shift
                [[ $# -eq 0 ]] || { printf 'only one revision is accepted\n' >&2; exit 2; }
                ;;
        esac
    done

    if [[ "$mode" == "local" ]]; then
        [[ -z "$repository_url" ]] || { printf '--local-source conflicts with --repository-url\n' >&2; exit 2; }
        [[ "$revision" == "HEAD" ]] || { printf '--local-source does not accept a revision\n' >&2; exit 2; }
        prepare_local_source
        printf 'External consumers use a temporary commit of the current worktree.\n'
    else
        if [[ "$mode" == "checkout" ]]; then
            [[ -z "$repository_url" ]] || { printf '--checked-out-repository conflicts with --repository-url\n' >&2; exit 2; }
            repository_url="file://$REPO_ROOT"
        elif [[ -z "$repository_url" ]]; then
            repository_url="$(git -C "$REPO_ROOT" remote get-url origin 2>/dev/null || true)"
        fi
        validate_repository_url "$repository_url"
        SOURCE_URL="$repository_url"
        SOURCE_REVISION="$(git -C "$REPO_ROOT" rev-parse --verify "${revision}^{commit}")"
        printf 'External consumers pin the selected repository at %s.\n' "$SOURCE_REVISION"
    fi

    work="$(mktemp -d)"
    cp -R "$FIXTURES_ROOT/." "$work/"

    local consumer
    for consumer in "${CONSUMERS[@]}"; do
        rewrite_dependencies "$work/$consumer/Cargo.toml"
        run_consumer "$consumer"
    done

    run_consumer kvim-syntax --features grammar-rust
    run_consumer kvim-syntax --features all-grammars
    run_consumer kvim-embed-worktree --features grammar-rust
    run_consumer kvim-embed-worktree --features all-grammars
    printf 'All independent external consumers passed.\n'
}

main "$@"
