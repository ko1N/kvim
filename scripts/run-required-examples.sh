#!/usr/bin/env bash
# Run the required example list owned by docs/embedding.md.

set -euo pipefail
IFS=$'\n\t'

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
readonly REPO_ROOT
readonly DOCUMENT="$REPO_ROOT/docs/embedding.md"
readonly REQUIRED_EXAMPLES_COUNT=21

main() {
    local records
    records="$(mktemp)"
    trap 'rm -f "$records"' RETURN

    python3 - "$REPO_ROOT" "$DOCUMENT" "$REQUIRED_EXAMPLES_COUNT" > "$records" <<'PY'
from pathlib import Path
import re
import sys
import tomllib

root = Path(sys.argv[1])
document = Path(sys.argv[2])
expected_count = int(sys.argv[3])
lines = document.read_text().splitlines()
try:
    marker = lines.index("The required examples are:")
except ValueError as error:
    raise SystemExit(f"{document}: required example list marker is missing") from error

paths: list[str] = []
index = marker + 1
while index < len(lines) and not lines[index]:
    index += 1
while index < len(lines):
    match = re.fullmatch(r"- `(crates/([^/]+)/examples/([^/]+)\.rs)`", lines[index])
    if match is None:
        break
    paths.append(match.group(1))
    index += 1

if index >= len(lines) or lines[index] != "":
    raise SystemExit(f"{document}: required example list has no blank terminator")
index += 1
if index >= len(lines) or not lines[index].startswith("Each example demonstrates"):
    raise SystemExit(f"{document}: required example list ended at an unexpected line")
if len(paths) != expected_count:
    raise SystemExit(
        f"{document}: expected {expected_count} required examples, found {len(paths)}"
    )
if len(set(paths)) != len(paths):
    raise SystemExit(f"{document}: required example list contains a duplicate")

for relative in paths:
    match = re.fullmatch(r"crates/([^/]+)/examples/([^/]+)\.rs", relative)
    assert match is not None
    package, example = match.groups()
    if not (root / relative).is_file():
        raise SystemExit(f"{document}: required example does not exist: {relative}")

    manifest_path = root / "crates" / package / "Cargo.toml"
    with manifest_path.open("rb") as manifest_file:
        manifest = tomllib.load(manifest_file)
    declarations = [
        entry for entry in manifest.get("example", []) if entry.get("name") == example
    ]
    if len(declarations) > 1:
        raise SystemExit(f"{manifest_path}: duplicate [[example]] declaration for {example}")
    features = declarations[0].get("required-features", []) if declarations else []
    if not isinstance(features, list) or not all(isinstance(item, str) for item in features):
        raise SystemExit(f"{manifest_path}: invalid required-features for {example}")
    print("\t".join((package, example, ",".join(features))))
PY

    local package example features
    while IFS=$'\t' read -r package example features; do
        printf '  %-15s %s\n' "$package" "$example"
        if [[ -n "$features" ]]; then
            cargo run --quiet --locked -p "$package" --example "$example" \
                --no-default-features --features "$features"
        else
            cargo run --quiet --locked -p "$package" --example "$example"
        fi
    done < "$records"

    printf 'Ran %d required examples from docs/embedding.md.\n' "$REQUIRED_EXAMPLES_COUNT"
}

main "$@"
