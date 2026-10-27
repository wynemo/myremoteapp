#!/usr/bin/env python3
"""Package the native diagnostic CLI and verify its executable architecture."""
import argparse
import hashlib
import os
from pathlib import Path
import struct
import tarfile
import tomllib
import zipfile

ROOT = Path(__file__).resolve().parents[1]
TARGETS = {
    "x86_64-pc-windows-msvc": ("windows-x64", "remote-cli.exe"),
    "aarch64-apple-darwin": ("macos-arm64", "remote-cli"),
}


def verify_binary(data: bytes, target: str) -> None:
    if target == "x86_64-pc-windows-msvc":
        if len(data) < 64 or data[:2] != b"MZ":
            raise ValueError("Expected a Windows PE executable")
        offset = struct.unpack_from("<I", data, 0x3C)[0]
        if offset + 6 > len(data) or data[offset:offset + 4] != b"PE\0\0":
            raise ValueError("Invalid Windows PE header")
        if struct.unpack_from("<H", data, offset + 4)[0] != 0x8664:
            raise ValueError("Expected a Windows x64 executable")
    else:
        if len(data) < 32 or data[:4] != b"\xcf\xfa\xed\xfe":
            raise ValueError("Expected a little-endian Mach-O 64 executable")
        if struct.unpack_from("<I", data, 4)[0] != 0x0100000C:
            raise ValueError("Expected a macOS arm64 executable")
        if struct.unpack_from("<I", data, 12)[0] != 2:
            raise ValueError("Expected a Mach-O executable")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", required=True, choices=TARGETS)
    args = parser.parse_args()
    platform, executable = TARGETS[args.target]
    with (ROOT / "Cargo.toml").open("rb") as manifest:
        version = tomllib.load(manifest)["workspace"]["package"]["version"]
    if os.environ.get("GITHUB_REF_TYPE") == "tag":
        tag = os.environ.get("GITHUB_REF_NAME")
        if tag != f"v{version}":
            raise ValueError(f"Release tag {tag!r} must match Cargo version v{version}")

    binary = ROOT / "target" / args.target / "release" / executable
    verify_binary(binary.read_bytes(), args.target)
    name = f"myremoteapp-{version}-{platform}"
    files = [
        (binary, executable),
        (ROOT / "readme.md", "readme.md"),
        (ROOT / "docs/protocol.md", "docs/protocol.md"),
    ]
    output = ROOT / "dist"
    output.mkdir(exist_ok=True)
    if args.target == "x86_64-pc-windows-msvc":
        archive = output / f"{name}.zip"
        with zipfile.ZipFile(archive, "w", zipfile.ZIP_DEFLATED) as package:
            for source, destination in files:
                package.write(source, f"{name}/{destination}")
    else:
        archive = output / f"{name}.tar.gz"
        with tarfile.open(archive, "w:gz") as package:
            for source, destination in files:
                info = package.gettarinfo(str(source), f"{name}/{destination}")
                info.mode = 0o755 if source == binary else 0o644
                with source.open("rb") as content:
                    package.addfile(info, content)
    checksum = hashlib.sha256(archive.read_bytes()).hexdigest()
    archive.with_name(archive.name + ".sha256").write_text(
        f"{checksum}  {archive.name}\n", encoding="utf-8"
    )
    if github_output := os.environ.get("GITHUB_OUTPUT"):
        with open(github_output, "a", encoding="utf-8") as outputs:
            outputs.write(f"version={version}\n")
    print(f"Packaged {archive.name}; SHA-256 {checksum}")


if __name__ == "__main__":
    main()
