#!/usr/bin/env bash
# Compare this crate's validator with NVIDIA's brep_validator.py on every .usda
# file in a usd-brep checkout, and check that our exports pass NVIDIA's validator.
#
#   ./run.sh /path/to/usd-brep
#
# Needs Python 3 with:  pip install usd-core "usd-validation-nvidia>=1.22.0,<2"
set -euo pipefail
REPO=$(cd "$1" && pwd)
HERE=$(cd "$(dirname "$0")" && pwd)
CRATE=$(cd "$HERE/../.." && pwd)
OUT=$(mktemp -d)
FILES=$(find "$REPO" -name '*.usda' -not -path '*/.git/*' | sort)
export OMNISOLID_PLUGIN_PATH="$REPO/source/schema/omniSolid/resources"

echo "== NVIDIA validator on all files"
python3 "$HERE/python_codes.py" "$REPO" $FILES > "$OUT/python.json" 2>/dev/null
echo "== Rust validator on all files"
(cd "$CRATE" && cargo run -q --release --example validate_files -- --json "$REPO" $FILES) > "$OUT/rust.json"
python3 "$HERE/compare.py" "$OUT/python.json" "$OUT/rust.json"

echo "== Round trip (read -> write -> read)"
(cd "$CRATE" && cargo run -q --release --example roundtrip -- $FILES) | tail -1

echo "== Rust exports checked by NVIDIA validator"
(cd "$CRATE" && cargo run -q --release --example demo -- "$OUT/exports") > /dev/null
python3 "$HERE/validate_runner.py" "$REPO" "$OUT"/exports/*.usda 2>/dev/null
