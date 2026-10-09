# Prepended to POSIX worker scripts. Uses non-interactive sudo when the host
# allows it, otherwise the sudo password supplied for this one operation.
run_as_root() {
  if sudo -n true 2>/dev/null; then
    sudo -n "$@"
  elif [ -n "${RF_SUDO_PASSWORD:-}" ]; then
    printf '%s\n' "$RF_SUDO_PASSWORD" | sudo -S -p '' "$@"
  else
    echo "non-interactive sudo is unavailable; enter the sudo password for this operation" >&2
    return 1
  fi
}
