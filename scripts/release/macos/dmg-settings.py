# dmgbuild settings for Luma's DMG: the app and an Applications link on the
# background dmg-background.py draws. The window size and icon centres here
# are the ones that script draws its tiles around; change both together.
#
# Called by bundle-mac as
#   dmgbuild -s dmg-settings.py -D app=<Luma.app> -D background=<png> Luma <out.dmg>
# dmgbuild picks up the @2x PNG beside `background` for Retina screens.
import os.path

app = defines["app"]  # noqa: F821 (dmgbuild's injected settings)
background = defines["background"]  # noqa: F821

format = "UDZO"
files = [app]
symlinks = {"Applications": "/Applications"}

window_rect = ((200, 120), (660, 420))
default_view = "icon-view"
show_status_bar = False
show_tab_view = False
show_toolbar = False
show_pathbar = False
show_sidebar = False

icon_size = 128
text_size = 13
icon_locations = {
    os.path.basename(app): (180, 196),
    "Applications": (480, 196),
}
