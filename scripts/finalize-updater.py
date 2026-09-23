"""Rebuild the updater manifest after every platform upload has completed."""
import json
import sys
from pathlib import Path

root = Path(sys.argv[1])
tag = sys.argv[2]
repo = sys.argv[3]
version = tag.removeprefix("v")
manifest_path = root / "latest.json"
manifest = json.loads(manifest_path.read_text())
assert manifest["version"].removeprefix("v") == version
platforms = {}
for keys, filename in [
    (["darwin-aarch64", "darwin-aarch64-app"], "Papr_aarch64.app.tar.gz"),
    (["darwin-x86_64", "darwin-x86_64-app"], "Papr_x64.app.tar.gz"),
    (["linux-x86_64", "linux-x86_64-appimage"], f"Papr_{version}_amd64.AppImage"),
    (["linux-x86_64-deb"], f"Papr_{version}_amd64.deb"),
    (["linux-x86_64-rpm"], f"Papr-{version}-1.x86_64.rpm"),
    (["windows-x86_64", "windows-x86_64-msi"], f"Papr_{version}_x64_en-US.msi"),
    (["windows-x86_64-nsis"], f"Papr_{version}_x64-setup.exe"),
]:
    signature = (root / (filename + ".sig")).read_text().strip()
    assert signature, f"Empty signature: {filename}"
    for key in keys:
        platforms[key] = {"signature": signature,
                          "url": f"https://github.com/{repo}/releases/download/{tag}/{filename}"}
manifest["platforms"] = platforms
manifest_path.write_text(json.dumps(manifest, indent=2) + "\n")
