#!/usr/bin/env python3
"""Download the Windows CLI from GitHub Actions using an authenticated gh CLI.

Requires Python 3.9+ and GitHub CLI (`gh auth login`, or set GH_TOKEN).
No third-party Python packages are required. The executable is never run.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import struct
import subprocess
import sys
import tempfile
import zipfile

ROOT = Path(__file__).resolve().parent
MAX_EXE_BYTES = 100 * 1024 * 1024


def gh(*args):
    result = subprocess.run(
        ["gh", *args], capture_output=True, text=True, timeout=180,
        env={**os.environ, "GH_HOST": "github.com", "GH_PROMPT_DISABLED": "1"},
    )
    if result.returncode:
        raise RuntimeError(result.stderr.strip() or "GitHub CLI request failed")
    return result.stdout


def api(endpoint, *fields):
    args = ["api", "--hostname", "github.com", "--method", "GET", endpoint]
    for field in fields:
        args.extend(["--raw-field", field])
    return json.loads(gh(*args))


def positive_id(value):
    number = int(value)
    if number <= 0:
        raise argparse.ArgumentTypeError("run ID must be positive")
    return number


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", default="wynemo/myremoteapp", help="GitHub OWNER/REPO")
    parser.add_argument("--branch", default="main", help="branch for the latest successful push build")
    parser.add_argument("--run-id", type=positive_id, help="download from this successful run instead")
    parser.add_argument("--output", type=Path, default=ROOT, help="output directory (default: repository root)")
    parser.add_argument("--force", action="store_true", help="replace an existing remote-cli.exe")
    args = parser.parse_args()
    if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", args.repo):
        parser.error("--repo must be OWNER/REPO")
    if shutil.which("gh") is None:
        raise RuntimeError("Install GitHub CLI first, then run: gh auth login")
    destination = args.output.expanduser().resolve() / "remote-cli.exe"
    if destination.exists() and not args.force:
        raise RuntimeError(f"{destination} already exists; use --force to replace it")

    base = f"repos/{args.repo}/actions"
    if args.run_id:
        run = api(f"{base}/runs/{args.run_id}")
    else:
        runs = api(
            f"{base}/workflows/ci.yml/runs", f"branch={args.branch}",
            "event=push", "status=success", "per_page=1",
        )["workflow_runs"]
        if not runs:
            raise RuntimeError(f"No successful push build found on {args.branch!r}")
        run = runs[0]
    if run["status"] != "completed" or run["conclusion"] != "success":
        raise RuntimeError("The selected workflow run has not completed successfully")
    run_id = run["id"]
    artifacts = api(f"{base}/runs/{run_id}/artifacts", "per_page=100")["artifacts"]
    matches = [item for item in artifacts if re.fullmatch(
        r"myremoteapp-.+-windows-x64", item["name"]
    ) and not item["expired"]]
    if len(matches) != 1:
        raise RuntimeError(
            f"Expected one Windows artifact in run {run_id}, found {len(matches)}; "
            "artifacts may have expired (30-day retention)"
        )
    artifact = matches[0]
    print(f"Run: {run['html_url']}", flush=True)
    print(f"Commit: {run['head_sha']}", flush=True)
    print(f"Downloading: {artifact['name']}", flush=True)
    with tempfile.TemporaryDirectory(prefix="myremoteapp-windows-") as temporary:
        gh("run", "download", str(run_id), "--repo", f"github.com/{args.repo}",
           "--name", artifact["name"], "--dir", temporary)
        archives = list(Path(temporary).glob("myremoteapp-*-windows-x64.zip"))
        if len(archives) != 1:
            raise RuntimeError("Artifact must contain exactly one Windows package ZIP")
        archive = archives[0]
        checksum_file = archive.with_name(archive.name + ".sha256")
        checksum_fields = checksum_file.read_text(encoding="utf-8").split()
        if len(checksum_fields) != 2 or checksum_fields[1] != archive.name:
            raise RuntimeError("Invalid SHA-256 checksum file")
        digest = hashlib.sha256(archive.read_bytes()).hexdigest()
        if digest != checksum_fields[0].lower():
            raise RuntimeError("Package SHA-256 mismatch; refusing to install")
        with zipfile.ZipFile(archive) as package:
            expected = f"{archive.stem}/remote-cli.exe"
            entries = [item for item in package.infolist() if item.filename == expected]
            if len(entries) != 1 or not 0 < entries[0].file_size <= MAX_EXE_BYTES:
                raise RuntimeError("Package must contain one remote-cli.exe under 100 MiB")
            executable = package.read(entries[0])
        if len(executable) < 64 or executable[:2] != b"MZ":
            raise RuntimeError("Not a Windows executable")
        offset = struct.unpack_from("<I", executable, 0x3C)[0]
        if (offset + 6 > len(executable)
                or executable[offset:offset + 4] != b"PE\0\0"
                or struct.unpack_from("<H", executable, offset + 4)[0] != 0x8664):
            raise RuntimeError("Not a Windows x64 PE executable")

        destination.parent.mkdir(parents=True, exist_ok=True)
        if args.force:
            # Stage beside the destination so replacement stays on one filesystem.
            with tempfile.NamedTemporaryFile(dir=destination.parent, delete=False) as staged:
                staged_path = Path(staged.name)
                try:
                    staged.write(executable)
                    staged.close()
                    os.replace(staged_path, destination)
                finally:
                    staged_path.unlink(missing_ok=True)
        else:
            with destination.open("xb") as output:
                try:
                    output.write(executable)
                except BaseException:
                    output.close()
                    destination.unlink(missing_ok=True)
                    raise
    print(f"Package SHA-256 verified: {digest}")
    print(f"Saved: {destination} ({len(executable):,} bytes)")


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, KeyError, RuntimeError, zipfile.BadZipFile,
            subprocess.TimeoutExpired) as error:
        print(f"Error: {error}", file=sys.stderr)
        sys.exit(1)
    except KeyboardInterrupt:
        sys.exit(130)
