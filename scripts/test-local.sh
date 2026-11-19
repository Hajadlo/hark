#!/usr/bin/env bash
# Run every hark test suite this host can run, without SSH to the desktop.
# Prints which suites ran, passed, failed or were skipped (and why).
# Exits non-zero if any suite that ran failed.
set -uo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
declare -a summary=()
failed=0

run() { # name, dir, command...
    local name=$1 dir=$2; shift 2
    printf '\n=== %s: %s\n' "$name" "$*"
    if (cd "$dir" && "$@"); then
        summary+=("PASS  $name")
    else
        summary+=("FAIL  $name")
        failed=1
    fi
}
skip() { summary+=("SKIP  $1: $2"); printf '\n=== %s: skipped (%s)\n' "$1" "$2"; }

# server: Python gateway
if command -v uv >/dev/null; then
    run server "$root/server" uv run -q --with pytest --with numpy pytest -q
else
    skip server "uv not on PATH"
fi

# linux: Rust client. cpal needs the ALSA headers (alsa-lib-devel) and the
# feedback/hotkey tests start a private dbus-daemon. Without the headers on the
# host, use the hark-build toolbox with the shared ~/.cargo rustup toolchain.
toolbox_setup="toolbox create -y --release 44 hark-build && toolbox run -c hark-build sudo dnf install -y alsa-lib-devel pkgconf-pkg-config gcc dbus-daemon"
if ! command -v cargo >/dev/null && [[ ! -x $HOME/.cargo/bin/cargo ]]; then
    skip linux "cargo not found (install: mise use -g rust@stable)"
elif pkg-config --exists alsa 2>/dev/null; then
    run linux "$root/linux" cargo test
elif command -v toolbox >/dev/null && podman container exists hark-build 2>/dev/null; then
    run linux "$root/linux" toolbox run -c hark-build \
        env PATH="$HOME/.cargo/bin:/usr/bin:/bin" cargo test
else
    skip linux "no ALSA headers on host and no hark-build toolbox; one-time setup: $toolbox_setup"
fi

# android: unit tests in the same container image the desktop uses; no device needed.
image=localhost/hark-android:agp9.4-sdk37
if ! command -v podman >/dev/null; then
    skip android "podman not on PATH"
else
    if ! podman image exists "$image"; then
        printf '\n=== android: building %s (one-off, several minutes)\n' "$image"
        podman build -t "$image" -f "$root/android/Containerfile" "$root/android" ||
            { summary+=("FAIL  android: image build"); failed=1; image=; }
    fi
    if [[ -n $image ]]; then
        mkdir -p "$HOME/.cache/hark-gradle"
        run android "$root/android" podman run --rm --userns keep-id --network host \
            -v "$root/android:/work:z" -v "$HOME/.cache/hark-gradle:/gradle:z" \
            -e GRADLE_USER_HOME=/gradle "$image" ./gradlew --no-daemon testDebugUnitTest
    fi
fi

printf '\n=== summary\n'
printf '%s\n' "${summary[@]}"
exit "$failed"
