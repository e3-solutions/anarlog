#!/usr/bin/env python3
"""Compare native build tools on disposable macOS checkouts."""

import argparse
import json
import os
from pathlib import Path
import platform
import shutil
import signal
import statistics
import subprocess
import sys
import time


VARIANTS = (
    "baseline-original",
    "baseline",
    "sccache",
    "sccache-no-incremental",
    "ccache",
    "lld",
    "line-tables",
    "swift-cache",
)


def capture(command, cwd, env):
    try:
        result = subprocess.run(
            command, cwd=cwd, env=env, capture_output=True, text=True, check=False
        )
        return {"exit_code": result.returncode, "output": result.stdout + result.stderr}
    except OSError as error:
        return {"exit_code": 127, "output": str(error)}


def benchmark(root, output, variant, samples, replay):
    root = root.resolve()
    output = output.resolve()
    if output.exists() and any(output.iterdir()):
        raise ValueError(f"Use a fresh output directory: {output}")
    output.mkdir(parents=True, exist_ok=True)
    source_root = root
    target = output / "target"

    env = os.environ.copy()
    cargo_bin = Path.home() / ".cargo/bin"
    if (
        not shutil.which("cargo", path=env.get("PATH"))
        and (cargo_bin / "cargo").exists()
    ):
        env["PATH"] = str(cargo_bin) + os.pathsep + env.get("PATH", "")
    xcode = Path("/Applications/Xcode.app/Contents/Developer")
    if (
        platform.system() == "Darwin"
        and not env.get("DEVELOPER_DIR")
        and xcode.exists()
    ):
        env["DEVELOPER_DIR"] = str(xcode)
    # All variants start from the same profile and empty cache, independent of CI caches.
    for key in tuple(env):
        if key.startswith(("CARGO_PROFILE_", "SCCACHE_", "CCACHE_")) or key in (
            "RUSTFLAGS",
            "CARGO_ENCODED_RUSTFLAGS",
            "RUSTC_WRAPPER",
            "RUSTC_WORKSPACE_WRAPPER",
            "CARGO_BUILD_RUSTC_WRAPPER",
            "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER",
            "CARGO_INCREMENTAL",
            "CC",
            "CXX",
            "CMAKE_C_COMPILER_LAUNCHER",
            "CMAKE_CXX_COMPILER_LAUNCHER",
        ):
            env.pop(key)
    env.update(
        CARGO_TARGET_DIR=str(target),
        CARGO_INCREMENTAL="1",
        CARGO_TERM_COLOR="never",
        RUSTC_WRAPPER="",
        RUSTC_WORKSPACE_WRAPPER="",
        RUSTFLAGS="",
    )
    plugin = root / "plugins/misc/src/lib.rs"
    manifest = root / "apps/desktop/src-tauri/Cargo.toml"
    originals = {plugin: plugin.read_bytes(), manifest: manifest.read_bytes()}
    needle = b'const PLUGIN_NAME: &str = "misc";'
    if originals[plugin].count(needle) != 1:
        raise ValueError("Cannot locate the native plugin edit fixture")

    result = {
        "variant": variant,
        "status": "running",
        "samples_requested": samples,
        "measurements": [],
        "metadata": {
            "platform": platform.platform(),
            "cpu_count": os.cpu_count(),
            "commit": capture(["git", "rev-parse", "HEAD"], root, env),
            "rustc": capture(["rustc", "-Vv"], root, env),
            "swift": capture(["swift", "--version"], root, env),
            "xcode": capture(["xcodebuild", "-version"], root, env),
            "metal": capture(["xcrun", "metal", "--version"], root, env),
            "memory": capture(["sysctl", "-n", "hw.memsize"], root, env),
        },
    }
    command = [
        "cargo",
        "build",
        "--locked",
        "-p",
        "desktop",
        "--features",
        "dev,direct-distribution",
        "--timings",
    ]
    result["command"] = command

    def save():
        (output / "results.json").write_text(json.dumps(result, indent=2) + "\n")

    def timed(label):
        print(f"Starting {variant}: {label}", flush=True)
        start = time.perf_counter()
        with (output / f"{label}.log").open("w") as log:
            proc = subprocess.Popen(
                command,
                cwd=root,
                env=env,
                stdout=log,
                stderr=subprocess.STDOUT,
                start_new_session=True,
            )
            try:
                code = proc.wait()
            except BaseException:
                os.killpg(proc.pid, signal.SIGTERM)
                try:
                    proc.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    os.killpg(proc.pid, signal.SIGKILL)
                    proc.wait()
                raise
        elapsed = time.perf_counter() - start
        timings = target / "cargo-timings"
        if timings.exists():
            shutil.copytree(timings, output / "timings" / label, dirs_exist_ok=True)
        result["measurements"].append(
            {"phase": label, "seconds": elapsed, "exit_code": code}
        )
        save()
        print(f"Finished {variant}: {label}: {elapsed:.2f}s (exit {code})", flush=True)
        if code:
            print(
                "\n".join(
                    (output / f"{label}.log")
                    .read_text(errors="replace")
                    .splitlines()[-60:]
                ),
                flush=True,
            )
            raise subprocess.CalledProcessError(code, command)

    try:
        if result["metadata"]["rustc"]["exit_code"]:
            result.update(
                status="blocked",
                reason="Install the pinned Rust toolchain with rustup before timing native builds",
            )
            return result
        if platform.system() == "Darwin" and "CARGO_BUILD_JOBS" not in env:
            memory = result["metadata"]["memory"]
            if memory["exit_code"] == 0 and memory["output"].strip().isdigit():
                env["CARGO_BUILD_JOBS"] = str(
                    min(
                        os.cpu_count() or 1,
                        max(1, int(memory["output"].strip()) // (4 * 1024**3)),
                    )
                )
        if variant != "swift-cache" and platform.system() == "Darwin":
            if (
                result["metadata"]["xcode"]["exit_code"]
                or result["metadata"]["metal"]["exit_code"]
            ):
                result.update(
                    status="blocked",
                    reason="Install compatible full Xcode and its Metal Toolchain before timing native builds",
                )
                return result
        if variant == "swift-cache":
            help_result = capture(["swift", "build", "--help"], root, env)
            (output / "swift-cache-capability.log").write_text(help_result["output"])
            if "--enable-build-caching" not in help_result["output"]:
                result.update(
                    status="unsupported",
                    reason="Installed SwiftPM does not expose --enable-build-caching",
                )
                return result
        # Copy tracked working files, including local edits and materialized LFS assets.
        # Never change the user's checkout, even if a build is interrupted.
        files = capture(
            ["git", "ls-files", "--recurse-submodules", "-z"], source_root, env
        )
        if files["exit_code"]:
            raise RuntimeError(files["output"])
        root = output / "checkout"
        root.mkdir()
        for name in filter(None, files["output"].split("\0")):
            relative = Path(name)
            if relative.is_absolute() or ".." in relative.parts:
                raise ValueError(f"Invalid tracked path: {name}")
            source = source_root / relative
            if not source.exists() and not source.is_symlink():
                continue
            destination = root / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source, destination, follow_symlinks=False)
        plugin = root / "plugins/misc/src/lib.rs"
        manifest = root / "apps/desktop/src-tauri/Cargo.toml"
        originals = {plugin: plugin.read_bytes(), manifest: manifest.read_bytes()}
        if variant == "baseline-original":
            before = originals[manifest]
            after = before.replace(
                b'crate-type = ["rlib"]', b'crate-type = ["staticlib", "rlib"]'
            )
            if after == before:
                raise ValueError(
                    "Cannot restore the original desktop archive configuration"
                )
            manifest.write_bytes(after)
        if variant.startswith("sccache") or variant == "ccache":
            tool = "sccache" if variant.startswith("sccache") else "ccache"
            executable = shutil.which(tool)
            if not executable:
                raise ValueError(f"{tool} is not installed")
            result["metadata"][tool] = capture([executable, "--version"], root, env)
            cache = output / "compiler-cache"
            if tool == "sccache":
                env.update(
                    RUSTC_WRAPPER=executable,
                    SCCACHE_DIR=str(cache),
                    SCCACHE_CACHE_SIZE="8G",
                    SCCACHE_SERVER_UDS=str(output / "sccache.sock"),
                )
            else:
                env.update(CCACHE_DIR=str(cache), CCACHE_MAXSIZE="8G")
            # cc-rs and CMake both accept compiler executable paths; Swift's own Clang jobs bypass these.
            for key, compiler in (("CC", "clang"), ("CXX", "clang++")):
                wrapper = output / key.lower()
                wrapper.write_text(f'#!/bin/sh\nexec "{executable}" {compiler} "$@"\n')
                wrapper.chmod(0o755)
                env[key] = str(wrapper)
            if variant == "sccache-no-incremental":
                env["CARGO_INCREMENTAL"] = "0"
        elif variant == "lld":
            lld = os.environ.get("BENCHMARK_LLD") or shutil.which("ld64.lld")
            if not lld or not Path(lld).is_file():
                raise ValueError(
                    "Set BENCHMARK_LLD to an installed Mach-O LLD executable"
                )
            env["RUSTFLAGS"] = f"-C link-arg=-fuse-ld={lld}"
            result["metadata"]["lld"] = capture([lld, "--version"], root, env)
        elif variant == "line-tables":
            env["CARGO_PROFILE_DEV_DEBUG"] = "line-tables-only"
        elif variant == "swift-cache":
            # Configure each package locally, leaving the runner's global Swift configuration intact.
            packages = [
                *root.glob("crates/*/swift-lib/Package.swift"),
                *root.glob("plugins/*/swift-lib/Package.swift"),
            ]
            for package in sorted(packages):
                configuration = capture(
                    [
                        "swift",
                        "package",
                        "--package-path",
                        str(package.parent),
                        "build-cache",
                        "configure",
                        "--enable-caching",
                        "--path",
                        str(output / "swift-cache"),
                    ],
                    root,
                    env,
                )
                if configuration["exit_code"]:
                    raise RuntimeError(configuration["output"])

        result["effective_settings"] = {
            key: env[key]
            for key in (
                "CARGO_TARGET_DIR",
                "CARGO_BUILD_JOBS",
                "CARGO_INCREMENTAL",
                "RUSTC_WRAPPER",
                "RUSTFLAGS",
                "CARGO_PROFILE_DEV_DEBUG",
                "CC",
                "CXX",
            )
            if key in env
        }
        timed("fresh-target")
        timed("unchanged")
        for sample in range(1, samples + 1):
            # Change compiled data in a real dependency, forcing its consumers to rebuild and relink.
            replacement = (
                f'const PLUGIN_NAME: &str = "misc-benchmark-{sample}";'.encode()
            )
            plugin.write_bytes(originals[plugin].replace(needle, replacement))
            timed(f"rust-edit-{sample}")
        edit_times = [
            m["seconds"]
            for m in result["measurements"]
            if m["phase"].startswith("rust-edit-")
        ]
        result["rust_edit_median_seconds"] = statistics.median(edit_times)
        if replay:
            plugin.write_bytes(originals[plugin])
            timed("restore-source")
            # Delete only this invocation's owned target directory; preserve the candidate's compiler cache.
            shutil.rmtree(target)
            timed("target-reset-cache-replay")
        result["status"] = "passed"
    except (OSError, ValueError, RuntimeError, subprocess.CalledProcessError) as error:
        result.update(status="failed", error=str(error))
    except (KeyboardInterrupt, SystemExit):
        result["status"] = "interrupted"
        raise
    finally:
        if variant.startswith("sccache"):
            result["cache_stats"] = capture(["sccache", "--show-stats"], root, env)
            if "SCCACHE_SERVER_UDS" in env:
                capture(["sccache", "--stop-server"], root, env)
        elif variant == "ccache":
            result["cache_stats"] = capture(["ccache", "--show-stats"], root, env)
        save()
        for owned in ("checkout", "target", "compiler-cache", "swift-cache"):
            shutil.rmtree(output / owned, ignore_errors=True)
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--root", type=Path, default=Path(__file__).resolve().parent.parent
    )
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--variant", choices=VARIANTS, required=True)
    parser.add_argument("--samples", type=int, default=3)
    parser.add_argument("--replay-cache", action="store_true")
    args = parser.parse_args()
    if args.samples < 1:
        parser.error("--samples must be positive")
    result = benchmark(
        args.root, args.output, args.variant, args.samples, args.replay_cache
    )
    print(json.dumps(result, indent=2))
    return {"failed": 1, "blocked": 2}.get(result["status"], 0)


if __name__ == "__main__":
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(143))
    sys.exit(main())
