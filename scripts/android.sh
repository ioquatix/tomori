#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
# Optional local SDK/toolchain paths; deliberately not committed.
if [[ -f .android-env ]]; then source .android-env; fi
: "${ANDROID_HOME:?Set ANDROID_HOME to the Android SDK directory}"
: "${ANDROID_NDK_HOME:?Set ANDROID_NDK_HOME to the Android NDK directory}"
export PATH="$ANDROID_HOME/platform-tools:$PATH"
package=dev.base.agentswarm
command="${1:-build}"
case "$command" in
    build|run)
        if [[ ! -s android/libs/arm64-v8a/libopenxr_loader.so ]]; then bash scripts/fetch-loader.sh; fi
        cargo metadata --locked --format-version 1 --no-deps > /dev/null
        cargo apk build --lib
        if [[ "$command" == run ]]; then
            adb install -r target/debug/apk/base-agent-swarm.apk
            adb reverse tcp:8765 "tcp:${BASE_SWARM_BRIDGE_PORT:-8765}"
            adb shell am start -n "$package/android.app.NativeActivity"
        fi
        ;;
    logs) adb logcat -s base-agent-swarm OpenXR-Loader AndroidRuntime ;;
    devices) adb devices -l ;;
    *) echo "Usage: $0 {build|run|logs|devices}" >&2; exit 2 ;;
esac
