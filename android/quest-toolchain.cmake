# whisper-rs forwards CMAKE_* variables, but the NDK toolchain expects the
# ANDROID_* names before it selects an ABI. Keep this explicit for Quest 3.
set(ANDROID_ABI arm64-v8a CACHE STRING "Quest ABI" FORCE)
set(ANDROID_PLATFORM android-29 CACHE STRING "Minimum Android API" FORCE)
set(ANDROID_STL c++_shared CACHE STRING "C++ runtime" FORCE)
include("$ENV{ANDROID_NDK_HOME}/build/cmake/android.toolchain.cmake")
