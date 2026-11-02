"""Check Init SDK package names and release versions before packaging (Python 3.11+)."""
import json
from pathlib import Path
import sys
import tomllib

root = Path(__file__).resolve().parents[1]
python = tomllib.loads((root / "sdk/python/pyproject.toml").read_text())["project"]
javascript = json.loads((root / "sdk/javascript/package.json").read_text())
rust = tomllib.loads((root / "sdk/rust/Cargo.toml").read_text())["package"]
names = {"Python": python["name"], "JavaScript": javascript["name"], "Rust": rust["name"]}
if any(name != "init-sdk" for name in names.values()):
    sys.exit(f"All SDK package names must be init-sdk: {names}")
versions = {
    "controller": tomllib.loads((root / "Cargo.toml").read_text())["package"]["version"],
    "Python": python["version"],
    "JavaScript": javascript["version"],
    "Rust": rust["version"],
}
expected = sys.argv[1] if len(sys.argv) > 1 else "v" + versions["controller"]
if any("v" + version != expected for version in versions.values()):
    sys.exit(f"SDK release versions must match {expected}: {versions}")
print(f"Init controller and all SDK versions match {expected}; package names are init-sdk")
