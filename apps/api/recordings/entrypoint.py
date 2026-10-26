import hashlib
import os
import pathlib
import signal
import subprocess
import sys
import time
import urllib.error
import urllib.request


MODELS = (
    (
        "ggml-tiny.en.bin",
        "https://huggingface.co/ggerganov/whisper.cpp/resolve/5359861c739e955e79d9a303bcbc70fb988958b1/ggml-tiny.en.bin",
        "921e4cf8686fdd993dcd081a5da5b6c365bfde1162e72b08d75ac75289920b1f",
    ),
    (
        "ggml-silero-v5.1.2.bin",
        "https://huggingface.co/ggml-org/whisper-vad/resolve/9ffd54a1e1ee413ddf265af9913beaf518d1639b/ggml-silero-v5.1.2.bin",
        "29940d98d42b91fbd05ce489f3ecf7c72f0a42f027e4875919a28fb4c04ea2cf",
    ),
)


def status(url):
    try:
        with urllib.request.urlopen(url, timeout=2) as response:
            return response.status
    except urllib.error.HTTPError as error:
        return error.code
    except (OSError, TimeoutError):
        return None


def digest(path):
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def models(directory):
    for filename, url, expected in MODELS:
        path = directory / filename
        if path.is_file() and digest(path) == expected:
            continue
        temporary = path.with_suffix(".download")
        try:
            with urllib.request.urlopen(url, timeout=60) as source:
                with temporary.open("wb") as target:
                    while chunk := source.read(1024 * 1024):
                        target.write(chunk)
            if digest(temporary) != expected:
                raise RuntimeError("model checksum mismatch")
            temporary.replace(path)
        finally:
            temporary.unlink(missing_ok=True)


def main():
    os.umask(0o077)
    port = int(os.environ.get("PORT", "8080"))
    if not 1024 <= port <= 65535:
        raise RuntimeError("PORT must be an unprivileged TCP port")
    if sys.argv[1:] == ["healthcheck"]:
        return 0 if status(f"http://127.0.0.1:{port}/health") == 200 else 1
    for name in (
        "SUPABASE_URL",
        "SUPABASE_ANON_KEY",
        "SUPABASE_SERVICE_ROLE_KEY",
        "NANGO_API_KEY",
        "NANGO_WEBHOOK_SIGNING_KEY",
    ):
        if not os.environ.get(name):
            raise RuntimeError(f"missing runtime variable {name}")
    root = pathlib.Path("/data")
    if not root.is_mount() or root.is_symlink():
        raise RuntimeError("mount a dedicated persistent volume at /data")
    for directory in (root, root / "recordings", root / "models", root / "runtime"):
        if directory.is_symlink():
            raise RuntimeError("data directories must not be symlinks")
        directory.mkdir(exist_ok=True)
        directory.chmod(0o700)
        if os.geteuid() == 0:
            os.chown(directory, 10001, 10001)
    if os.geteuid() == 0:
        os.setgroups([])
        os.setgid(10001)
        os.setuid(10001)
    os.environ.update(
        PORT=str(port),
        HOME="/data/runtime",
        XDG_DATA_HOME="/data/runtime",
        XDG_CONFIG_HOME="/data/runtime",
        ANARLOG_SHARED_RECORDINGS_DIR="/data/recordings",
        ANARLOG_SERVICE="core",
        ANARLOG_SHARED_RECORDINGS_TRANSCRIPTION_URL="http://127.0.0.1:18796/v1",
        ANARLOG_SHARED_RECORDINGS_TRANSCRIPTION_MODEL="tiny.en",
        ANARLOG_SHARED_RECORDINGS_TRANSCRIPTION_KEY="",
    )
    models(root / "models")
    children = []
    stopping = False

    def stop(_signal, _frame):
        nonlocal stopping
        stopping = True

    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)

    def spawn(command, private=False, backend=False):
        environment = (
            os.environ.copy()
            if backend
            else {
                key: os.environ[key]
                for key in ("PATH", "HOME", "XDG_DATA_HOME", "XDG_CONFIG_HOME", "PORT")
                if key in os.environ
            }
        )
        process = subprocess.Popen(
            command,
            env=environment,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL if private else None,
            stderr=subprocess.DEVNULL if private else None,
            start_new_session=True,
        )
        children.append(process)
        return process

    def ready(url, expected):
        deadline = time.monotonic() + 180
        while not stopping and time.monotonic() < deadline:
            if any(child.poll() is not None for child in children):
                raise RuntimeError("recording dependency exited during startup")
            if status(url) == expected:
                return
            time.sleep(1)
        raise RuntimeError("recording dependency startup timed out")

    try:
        spawn(
            [
                "whisper-server",
                "--host",
                "127.0.0.1",
                "--port",
                "18796",
                "--model",
                "/data/models/ggml-tiny.en.bin",
                "--threads",
                "2",
                "--no-gpu",
                "--vad",
                "--vad-model",
                "/data/models/ggml-silero-v5.1.2.bin",
                "--inference-path",
                "/v1/audio/transcriptions",
            ],
            private=True,
        )
        ready("http://127.0.0.1:18796/health", 200)
        spawn(["api"], backend=True)
        ready(f"http://127.0.0.1:{port}/health", 200)
        print("Anarlog API with local transcription ready", flush=True)
        while not stopping:
            if any(child.poll() is not None for child in children):
                raise RuntimeError("recording dependency exited")
            time.sleep(1)
        return 0
    finally:
        # Stop the public API before the private ASR.
        for child in reversed(children):
            if child.poll() is None:
                try:
                    os.killpg(child.pid, signal.SIGINT)
                except ProcessLookupError:
                    pass
        deadline = time.monotonic() + 10
        for child in reversed(children):
            try:
                child.wait(timeout=max(0.01, deadline - time.monotonic()))
            except subprocess.TimeoutExpired:
                try:
                    os.killpg(child.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                child.wait(timeout=2)


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Exception as error:
        # Runtime environment and ASR output may contain credentials or room speech.
        print(f"API stopped: {type(error).__name__}", file=sys.stderr, flush=True)
        sys.exit(1)
