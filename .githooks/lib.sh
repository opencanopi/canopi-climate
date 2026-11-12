# Shared by the hooks (sourced, not run). `capped <cmd>` runs a command in a systemd scope capped at 3 GB of
# memory with no swap, and at 10 minutes, when systemd-run exists; plainly otherwise (CI, macOS).
# JOBS: cargo build jobs (default 4; the machine is shared).
JOBS=${CANOPI_HOOK_JOBS:-4}
capped() {
  if command -v systemd-run >/dev/null 2>&1 && systemd-run --user --scope -q true >/dev/null 2>&1; then
    timeout 600 systemd-run --user --scope -q -p MemoryMax=3G -p MemorySwapMax=0 "$@"
  else
    "$@"
  fi
}
