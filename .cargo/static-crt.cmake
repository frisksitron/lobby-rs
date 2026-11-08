# opusic-sys builds libopus against the DLL C runtime unless told otherwise, which would put the
# Visual C++ runtime DLL back into the static build. config.toml points CMake here for Windows.
set(OPUS_STATIC_RUNTIME ON CACHE BOOL "" FORCE)
