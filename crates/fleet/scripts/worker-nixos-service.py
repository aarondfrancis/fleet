"""Install or remove one generated native service; invoked as root through SSH stdin."""
from __future__ import annotations

import json
import os
from pathlib import Path
import pwd
import re
import subprocess
import sys

NIXOS_DIR = Path("/etc/nixos")


def render_service(root: str, name: str, user: str) -> str:
    for value in (name, user):
        if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]*", value):
            raise ValueError("unsafe runner or user name")
    if any(char in root for char in ('$', '%', '\n', '\r', '\\', '"')):
        raise ValueError("unsafe runner path")
    q = json.dumps
    return f'''# Generated from runner-fleet inventory. Contains no credentials.
{{ pkgs, ... }}: {{
  systemd.services.{q("runner-fleet-" + name)} = {{
    description = {q("GitHub Actions runner " + name)};
    wantedBy = [ "multi-user.target" ];
    after = [ "network-online.target" ];
    wants = [ "network-online.target" ];
    unitConfig.ConditionPathExists = {q(root + "/.fleet-service-ready")};
    path = with pkgs; [ bash coreutils git ];
    environment = {{
      NIX_LD_LIBRARY_PATH = "/run/current-system/sw/share/nix-ld/lib";
      LD_LIBRARY_PATH = "/run/current-system/sw/share/nix-ld/lib";
    }};
    serviceConfig = {{
      User = {q(user)};
      WorkingDirectory = {q(root)};
      ExecStart = {q("/run/current-system/sw/bin/bash " + root + "/bin/runsvc.sh")};
      KillMode = "process";
      KillSignal = "SIGTERM";
      TimeoutStopSec = "5min";
    }};
  }};
}}
'''


def _checked_runner(root: str, name: str, user: str):
    content = render_service(root, name, user)
    account = pwd.getpwnam(user)
    path = Path(root)
    if path.resolve() != path or not path.is_relative_to(Path(account.pw_dir) / "gh-runners"):
        raise ValueError("runner path must be exact and beneath the service user's gh-runners")
    if json.loads((path / ".runner").read_text(encoding="utf-8-sig")).get("agentName") != name:
        raise ValueError("configured runner name does not match inventory")
    return content, account, path


def install(root: str, name: str, user: str) -> None:
    content, account, path = _checked_runner(root, name, user)
    unit = "runner-fleet-" + name + ".service"
    service_file = path / ".service"
    if service_file.exists() and service_file.read_text().strip() != unit:
        raise ValueError("refusing to replace an existing different service")
    module = NIXOS_DIR / ("runner-fleet-" + name + ".nix")
    if module.exists() and module.read_text() != content:
        raise ValueError("existing NixOS service module differs; review before changing")
    config = NIXOS_DIR / "configuration.nix"
    source = config.read_text()
    entry = "./" + module.name
    if entry not in source:
        if source.count("imports = [") != 1:
            raise ValueError("cannot locate a unique NixOS imports list")
        backup = config.with_name("configuration.nix.before-" + name)
        if backup.exists():
            raise ValueError("refusing to overwrite configuration backup")
        backup.write_text(source)
        module.write_text(content)
        config.write_text(source.replace("imports = [", "imports = [\n    " + entry, 1))
    else:
        module.write_text(content)
    # A fresh service cannot start at switch/reboot until the controller opens
    # the marker after checking trusted-group membership and labels.
    subprocess.run(["nixos-rebuild", "switch"], check=True)
    service_file.write_text(unit + "\n")
    os.chown(service_file, account.pw_uid, account.pw_gid)


def uninstall(root: str, name: str, user: str) -> None:
    """Reverse install() for one exact runner; the runner directory is preserved."""
    content, _account, path = _checked_runner(root, name, user)
    unit = "runner-fleet-" + name + ".service"
    service_file = path / ".service"
    if service_file.read_text().strip() != unit:
        raise ValueError("runner is not managed by the expected generated service")
    module = NIXOS_DIR / ("runner-fleet-" + name + ".nix")
    if module.exists() and module.read_text() != content:
        raise ValueError("existing NixOS service module differs; review before removing")
    config = NIXOS_DIR / "configuration.nix"
    source = config.read_text()
    inserted = "\n    ./" + module.name
    if source.count(inserted) > 1:
        raise ValueError("NixOS imports list contains the module more than once; review manually")
    if inserted in source:
        backup = config.with_name("configuration.nix.before-decommission-" + name)
        if backup.exists():
            raise ValueError("refusing to overwrite configuration backup")
        backup.write_text(source)
        config.write_text(source.replace(inserted, "", 1))
    elif "./" + module.name in source:
        raise ValueError("module import is not in the generated form; remove it manually")
    if module.exists():
        module.unlink()
    subprocess.run(["nixos-rebuild", "switch"], check=True)
    (path / ".fleet-service-ready").unlink(missing_ok=True)
    service_file.unlink()


if __name__ == "__main__":
    if sys.argv[1:2] == ["uninstall"]:
        uninstall(*sys.argv[2:])
    else:
        install(*sys.argv[1:])
