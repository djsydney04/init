"""Check SDK versions against a release tag before packaging (Python 3.11+)."""
import json
from pathlib import Path
import sys
import tomllib

root = Path(__file__).resolve().parents[1]
versions = {
    "controller": tomllib.loads((root / "Cargo.toml").read_text())["package"]["version"],
    "Python": tomllib.loads((root / "sdk/python/pyproject.toml").read_text())["project"]["version"],
    "JavaScript": json.loads((root / "sdk/javascript/package.json").read_text())["version"],
    "Rust": tomllib.loads((root / "sdk/rust/Cargo.toml").read_text())["package"]["version"],
}
expected = sys.argv[1] if len(sys.argv) > 1 else "v" + versions["controller"]
if any("v" + version != expected for version in versions.values()):
    sys.exit(f"SDK release versions must match {expected}: {versions}")
print(f"Controller and all SDK versions match {expected}")
