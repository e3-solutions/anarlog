import importlib.util
import json
import plistlib
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch


spec = importlib.util.spec_from_file_location(
    "local_signer", Path(__file__).with_name("sign-e3-pilot-local.py")
)
signer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(signer)


class LocalPilotSigningTests(unittest.TestCase):
    def test_signing_preserves_source_and_entitlements_and_archives_after_optional_stapling(
        self,
    ):
        with tempfile.TemporaryDirectory(prefix="pilot signing test ") as temporary:
            root = Path(temporary).resolve()
            source = root / "Source Pilot.app"
            main = "Contents/MacOS/e3-meeting-capture-pilot"
            code_paths = [
                main,
                "Contents/MacOS/anarlog-cli",
                "Contents/MacOS/check-permissions",
                "Contents/MacOS/char-chrome-native-host",
                "Contents/Frameworks/cloudsync.dylib",
            ]
            for relative in code_paths:
                path = source / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(b"\xcf\xfa\xed\xfeoriginal")
            (source / "Contents/Info.plist").write_bytes(
                plistlib.dumps(
                    {
                        "CFBundleIdentifier": signer.PILOT_IDENTIFIER,
                        "CFBundleExecutable": Path(main).name,
                    }
                )
            )
            original = {
                p.relative_to(source): p.read_bytes()
                for p in source.rglob("*")
                if p.is_file()
            }
            keychain = root / "custom signing.keychain-db"
            keychain.touch()
            identity = "Developer ID Application: Fixture (TEAMID)"
            expected = {
                Path(p).name: {"fixture-entitlement": Path(p).name} for p in code_paths
            }
            expected["cloudsync.dylib"] = {}
            for profile in [None, "existing-profile"]:
                with self.subTest(profile=profile):
                    output = root / ("notarized output" if profile else "signed output")
                    calls, signed = [], []
                    source_attributes = {
                        "com.apple.FinderInfo": b"finder",
                        "com.apple.ResourceFork": b"resource",
                        "com.apple.quarantine": b"quarantine",
                        "com.apple.provenance": b"provenance",
                    }
                    copied_attributes = dict(source_attributes)

                    def command(args):
                        calls.append(args)
                        tool = Path(args[0]).name
                        result = subprocess.CompletedProcess(
                            args, 0, stdout=b"", stderr=b""
                        )
                        if tool == "ditto":
                            if "-c" in args:
                                Path(args[-1]).write_bytes(b"archive")
                            else:
                                shutil.copytree(args[-2], args[-1], symlinks=True)
                        elif tool == "xattr":
                            self.assertEqual(args[1:4], ["-r", "-s", "-d"])
                            self.assertEqual(Path(args[-1]), output / source.name)
                            self.assertIn(
                                args[4],
                                {"com.apple.FinderInfo", "com.apple.ResourceFork"},
                            )
                            copied_attributes.pop(args[4], None)
                        elif tool == "codesign" and "-d" in args:
                            result.stdout = plistlib.dumps(
                                expected[Path(args[-1]).name]
                            )
                        elif tool == "codesign" and "--force" in args:
                            target = Path(args[-1])
                            self.assertNotIn(source, target.parents)
                            self.assertEqual(
                                args[args.index("--keychain") + 1], str(keychain)
                            )
                            self.assertNotIn("--preserve-metadata", args)
                            code = target / main if target.suffix == ".app" else target
                            if expected[code.name]:
                                actual = plistlib.loads(
                                    Path(
                                        args[args.index("--entitlements") + 1]
                                    ).read_bytes()
                                )
                                self.assertEqual(actual, expected[code.name])
                            else:
                                self.assertNotIn("--entitlements", args)
                            code.write_bytes(code.read_bytes() + b"signed")
                            signed.append(code)
                        elif tool == "xcrun":
                            self.assertIsNotNone(profile)
                            if "notarytool" in args:
                                result.stdout = json.dumps(
                                    {"status": "Accepted"}
                                ).encode()
                        return result

                    with patch.object(signer, "command", command):
                        result = signer.sign_pilot(
                            source, output, identity, keychain, profile
                        )
                    self.assertEqual(signed[-1], Path(result["app"]) / main)
                    self.assertEqual(len(signed), 5)
                    self.assertEqual(result["notarized"], bool(profile))
                    self.assertEqual(
                        copied_attributes,
                        {
                            "com.apple.quarantine": b"quarantine",
                            "com.apple.provenance": b"provenance",
                        },
                    )
                    self.assertIn("com.apple.FinderInfo", source_attributes)
                    self.assertIn("com.apple.ResourceFork", source_attributes)
                    self.assertTrue(Path(result["zip"]).is_file())
                    self.assertEqual(
                        original,
                        {
                            p.relative_to(source): p.read_bytes()
                            for p in source.rglob("*")
                            if p.is_file()
                        },
                    )
                    if profile:
                        staple = next(
                            i for i, args in enumerate(calls) if "staple" in args
                        )
                        self.assertEqual(calls[-1][0], "/usr/bin/ditto")
                        self.assertGreater(len(calls) - 1, staple)
                    with self.assertRaises(ValueError):
                        signer.sign_pilot(source, output, identity, keychain, profile)
            outside = root / "outside-file"
            outside.write_bytes(b"outside untouched")
            (source / "Contents/ExternalLink").symlink_to(outside)
            calls.clear()
            with patch.object(signer, "command", command), self.assertRaises(
                ValueError
            ):
                signer.sign_pilot(
                    source, root / "external-link output", identity, keychain
                )
            self.assertEqual(outside.read_bytes(), b"outside untouched")
            self.assertFalse(
                any(
                    Path(args[0]).name == "xattr" or "--force" in args for args in calls
                )
            )


if __name__ == "__main__":
    unittest.main()
