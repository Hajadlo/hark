#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
binary=${1:-"$root/target/release/hark"}
config_dir=${XDG_CONFIG_HOME:-"$HOME/.config"}
install -Dm755 "$binary" "$HOME/.local/bin/hark"
install -Dm644 "$root/hark.service" "$config_dir/systemd/user/app-cz.hajadlo.hark.service"
mkdir -p "$HOME/.local/share/applications"
sed "s|Exec=hark run|Exec=$HOME/.local/bin/hark run|" "$root/cz.hajadlo.hark.desktop" > "$HOME/.local/share/applications/cz.hajadlo.hark.desktop"
chmod 644 "$HOME/.local/share/applications/cz.hajadlo.hark.desktop"
mkdir -p "$config_dir/hark"
if [[ ! -e "$config_dir/hark/config.toml" ]]; then
    install -m600 "$root/config.example.toml" "$config_dir/hark/config.toml"
else
    [[ $(stat -c %a "$config_dir/hark/config.toml") == 600 ]] || printf '%s\n' 'Warning: fixing config permissions to 600.' >&2
    chmod 600 "$config_dir/hark/config.toml"
fi
command -v update-desktop-database >/dev/null && update-desktop-database "$HOME/.local/share/applications"
systemctl --user daemon-reload
printf '%s\n' 'Installed app-cz.hajadlo.hark.service; it is not enabled or started.' 'Set the token in the config, quit Handy, run hark check, then start the unit.'
