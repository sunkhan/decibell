Decibell for Linux (x86_64), as a plain directory
================================================

Run it from here:      ./decibell
It needs: a Vulkan driver (or OpenGL), libxkbcommon, libxcb, libva, libdrm, libopus, PipeWire and
ALSA libraries — every desktop distribution has them.

This copy doesn't update itself: About tells you when a new version is out and opens its release page
(the AppImage updates itself; the AUR and .deb packages update through the package manager).

Nothing is registered on your system by the program. To get a launcher entry and decibell:// invite
links, install the desktop entry and the icons, with the Exec line pointing here, e.g. for your user:

    dir="$(pwd)"
    mkdir -p ~/.local/share/applications
    sed "s|^Exec=decibell|Exec=$dir/decibell|" share/applications/decibell.desktop \
        > ~/.local/share/applications/decibell.desktop
    cp -r share/icons ~/.local/share/
    update-desktop-database ~/.local/share/applications

Your data lives in ~/.config/decibell. The licences of what this program contains are in
licenses/THIRD_PARTY_LICENSES.md.
