#!/usr/bin/python3
"""Sign a copy of an existing E3 pilot; no compilation or credential import.

Requires an unlocked Developer ID keychain. --notary-profile uses an existing
notarytool profile in its default keychain; no passwords are accepted here.
"""

import argparse
import json
import plistlib
import subprocess
import sys
import tempfile
from pathlib import Path


MACHO_MAGIC = {
    b"\xfe\xed\xfa\xce",
    b"\xce\xfa\xed\xfe",
    b"\xfe\xed\xfa\xcf",
    b"\xcf\xfa\xed\xfe",
    b"\xca\xfe\xba\xbe",
    b"\xbe\xba\xfe\xca",
    b"\xca\xfe\xba\xbf",
    b"\xbf\xba\xfe\xca",
}
PILOT_IDENTIFIER = "ai.e3group.meeting-capture-pilot"


def command(arguments):
    return subprocess.run(arguments, check=True, capture_output=True)


def entitlements(code):
    result = command(
        ["/usr/bin/codesign", "-d", "--entitlements", "-", "--xml", str(code)]
    )
    return plistlib.loads(result.stdout) if result.stdout.strip() else {}


def sign_pilot(input_app, output_dir, identity, keychain=None, notary_profile=None):
    source = Path(input_app).resolve(strict=True)
    output = Path(output_dir).resolve()
    if not source.is_dir() or source.suffix != ".app":
        raise ValueError("Input must be an existing .app bundle")
    if output == source or source in output.parents:
        raise ValueError("Output directory must be outside the source application")
    info = plistlib.loads((source / "Contents/Info.plist").read_bytes())
    if info.get("CFBundleIdentifier") != PILOT_IDENTIFIER:
        raise ValueError("Input is not the isolated E3 meeting capture pilot")
    executable = info.get("CFBundleExecutable", "")
    if not executable or Path(executable).name != executable:
        raise ValueError("Invalid main executable in Info.plist")
    if not identity.startswith("Developer ID Application:") and not (
        len(identity) == 40
        and all(character in "0123456789abcdefABCDEF" for character in identity)
    ):
        raise ValueError("Use the Developer ID Application certificate name or SHA-1")
    signing_keychain = Path(keychain).resolve(strict=True) if keychain else None
    app = output / source.name
    archive = output / (source.stem + "-DeveloperID.zip")
    if app.exists() or app.is_symlink() or archive.exists() or archive.is_symlink():
        raise ValueError(
            "Output app/archive already exists; choose a new output directory"
        )
    output.mkdir(parents=True, exist_ok=True)
    command(["/usr/bin/ditto", str(source), str(app)])
    main = app / "Contents/MacOS" / executable
    code_objects = []
    for path in (app / "Contents").rglob("*"):
        if path.is_symlink():
            if app not in path.resolve().parents:
                raise ValueError(
                    "Bundle contains a symlink outside its copied application"
                )
            continue
        if path.is_file():
            with path.open("rb") as content:
                if content.read(4) in MACHO_MAGIC:
                    code_objects.append(path)
    if main not in code_objects:
        raise ValueError("Main executable is not a Mach-O file")
    # The pilot contains loose sidecars and a dylib, not nested code bundles.
    if any(
        path.is_dir() and path.suffix in {".app", ".framework", ".xpc", ".appex"}
        for path in (app / "Contents").rglob("*")
    ):
        raise ValueError(
            "Nested code bundles require their own inside-out signing support"
        )
    # ditto can retain Finder metadata that codesign rejects. Only these two
    # attributes are removed, from the copy; quarantine/provenance remain intact.
    # Recursive deletion ignores absent attributes. -s never follows symlinks.
    for attribute in ("com.apple.FinderInfo", "com.apple.ResourceFork"):
        command(["/usr/bin/xattr", "-r", "-s", "-d", attribute, str(app)])
    preserved = {path: entitlements(path) for path in code_objects}
    signing = [
        "/usr/bin/codesign",
        "--force",
        "--sign",
        identity,
        "--options",
        "runtime",
        "--timestamp",
    ]
    if signing_keychain:
        signing += ["--keychain", str(signing_keychain)]
    # Do not preserve old ad-hoc requirements: codesign derives Developer ID
    # designated requirements from this certificate and the unchanged bundle ID.
    nested = sorted(
        (path for path in code_objects if path != main),
        key=lambda path: len(path.parts),
        reverse=True,
    )
    with tempfile.TemporaryDirectory(prefix="e3-pilot-entitlements-") as temporary:
        for index, code in enumerate(nested + [main]):
            arguments = list(signing)
            if preserved[code]:
                plist = Path(temporary) / (str(index) + ".plist")
                plist.write_bytes(plistlib.dumps(preserved[code]))
                arguments += ["--entitlements", str(plist)]
            # Signing the outer app signs its main executable and seals resources.
            command(arguments + [str(app if code == main else code)])
        for code, expected in preserved.items():
            if entitlements(code) != expected:
                raise RuntimeError(
                    "Entitlements changed while signing " + str(code.relative_to(app))
                )
    command(
        ["/usr/bin/codesign", "--verify", "--deep", "--strict", "--verbose=2", str(app)]
    )
    requirement = "=anchor apple generic and certificate leaf[field.1.2.840.113635.100.6.1.13] exists"
    command(
        [
            "/usr/bin/codesign",
            "--verify",
            "--strict",
            "--test-requirement",
            requirement,
            str(app),
        ]
    )
    command(
        [
            "/usr/bin/ditto",
            "-c",
            "-k",
            "--sequesterRsrc",
            "--keepParent",
            str(app),
            str(archive),
        ]
    )
    if notary_profile:
        submitted = command(
            [
                "/usr/bin/xcrun",
                "notarytool",
                "submit",
                str(archive),
                "--keychain-profile",
                notary_profile,
                "--wait",
                "--timeout",
                "30m",
                "--output-format",
                "json",
            ]
        )
        response = json.loads(submitted.stdout)
        if response.get("status") != "Accepted":
            raise RuntimeError(
                "Notarization was not Accepted; inspect the submission before retrying"
            )
        command(["/usr/bin/xcrun", "stapler", "staple", str(app)])
        command(["/usr/bin/xcrun", "stapler", "validate", str(app)])
        command(
            [
                "/usr/sbin/spctl",
                "--assess",
                "--type",
                "execute",
                "--verbose=4",
                str(app),
            ]
        )
        archive.unlink()
        command(
            [
                "/usr/bin/ditto",
                "-c",
                "-k",
                "--sequesterRsrc",
                "--keepParent",
                str(app),
                str(archive),
            ]
        )
    return {
        "app": str(app),
        "zip": str(archive),
        "bundle_identifier": PILOT_IDENTIFIER,
        "notarized": bool(notary_profile),
        "signed_code_objects": len(code_objects),
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("input_app", type=Path)
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument(
        "--identity",
        required=True,
        help="Developer ID Application certificate name or SHA-1",
    )
    parser.add_argument(
        "--keychain", type=Path, help="Unlocked keychain holding the signing identity"
    )
    parser.add_argument(
        "--notary-profile", help="Existing notarytool profile in the default keychain"
    )
    args = parser.parse_args()
    if sys.platform != "darwin":
        parser.error("This script requires macOS")
    try:
        result = sign_pilot(
            args.input_app,
            args.output_dir,
            args.identity,
            args.keychain,
            args.notary_profile,
        )
    except subprocess.CalledProcessError as error:
        # Commands use only certificate names/profile names, never passwords.
        detail = error.stderr.decode(errors="replace").strip() if error.stderr else ""
        raise SystemExit("Signing command failed: " + (detail or str(error.returncode)))
    except (OSError, ValueError, RuntimeError) as error:
        raise SystemExit(str(error))
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
