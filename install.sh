#!/bin/sh
# Installs Cardioid for the current user and starts it at login.
# ./install.sh --uninstall removes it again. Settings stay in ~/.config/cardioid.
set -eu
cd "$(dirname "$0")"

bin="$HOME/.local/bin/cardioid"
apps="$HOME/.local/share/applications"
icons="$HOME/.local/share/icons/hicolor/scalable/apps"
autostart="${XDG_CONFIG_HOME:-$HOME/.config}/autostart"

if [ "${1:-}" = "--uninstall" ]; then
    pkill -x cardioid || true
    rm -f "$bin" "$apps/cardioid.desktop" "$autostart/cardioid.desktop" "$icons/cardioid.svg"
    echo "Removed Cardioid."
    exit 0
fi

for tool in cargo pipewire pw-cli pw-dump pw-metadata pw-record pw-play pactl; do
    command -v "$tool" >/dev/null || { echo "Missing $tool, see Requirements in the README." >&2; exit 1; }
done
if command -v pacman >/dev/null; then
    missing=$(pacman -T lsp-plugins-lv2 calf noise-suppression-for-voice || true)
    if [ -n "$missing" ]; then
        echo "Install these first: sudo pacman -S $(echo "$missing" | tr '\n' ' ')" >&2
        exit 1
    fi
fi

cargo build --release

mkdir -p "$(dirname "$bin")" "$apps" "$icons" "$autostart"
pkill -x cardioid || true
install -m755 target/release/cardioid "$bin"
install -m644 assets/cardioid.svg "$icons/"

entry() {
    cat <<DESKTOP
[Desktop Entry]
Type=Application
Name=Cardioid
Comment=Filtered virtual microphone for OBS, Discord and other apps
Exec=$1
Icon=cardioid
Categories=AudioVideo;Audio;Mixer;
StartupWMClass=cardioid
DESKTOP
}
entry "$bin" > "$apps/cardioid.desktop"
entry "$bin --background" > "$autostart/cardioid.desktop"

# Running KDE processes index icon folders at startup, so a new icon stays
# missing until they rescan. Must happen before the tray item registers.
dbus-send --session --type=signal /KIconLoader org.kde.KIconLoader.iconChanged int32:0 2>/dev/null || true

"$bin" --background >/dev/null 2>&1 &
echo "Installed. Cardioid is running and starts at login."
