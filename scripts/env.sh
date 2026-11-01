# Local build environment (sourced by scripts). Host-specific linker only when no cc exists.
export CARGO_HOME="${CARGO_HOME:-/data/.cargo}" RUSTUP_HOME="${RUSTUP_HOME:-/data/.rustup}"
export PATH="$CARGO_HOME/bin:$PATH"
export CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
if ! command -v cc >/dev/null 2>&1 && [ -x /data/.local/bin/lld-cc ]; then
  export CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=/data/.local/bin/lld-cc
fi
