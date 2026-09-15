import json
import sys

import dbus
import dbus.mainloop.glib
import dbus.service
import gi

gi.require_version("Gtk", "3.0")
gi.require_version("Gdk", "3.0")
from gi.repository import Gdk, GLib, Gtk

terminal = sys.argv[1] == "terminal"
GLib.set_prgname("transcribe-terminal-fixture" if terminal else "transcribe-input-fixture")
dbus.mainloop.glib.DBusGMainLoop(set_as_default=True)
bus = dbus.SessionBus()
window = Gtk.Window(title="Transcribe Linux input test")
window.set_default_size(520, 180)
view = Gtk.TextView()
window.add(view)
keys = []


def pressed(widget, event):
    keys.append(Gdk.keyval_name(event.keyval))
    if terminal and event.keyval in (Gdk.KEY_v, Gdk.KEY_V):
        modifiers = event.state & (Gdk.ModifierType.CONTROL_MASK | Gdk.ModifierType.SHIFT_MASK)
        if modifiers == (Gdk.ModifierType.CONTROL_MASK | Gdk.ModifierType.SHIFT_MASK):
            view.get_buffer().paste_clipboard(Gtk.Clipboard.get(Gdk.SELECTION_CLIPBOARD), None, True)
        return True
    return False


view.connect("key-press-event", pressed)


class Fixture(dbus.service.Object):
    @dbus.service.method("app.transcribe.TestFixture", out_signature="s")
    def state(self):
        buffer = view.get_buffer()
        return json.dumps({"text": buffer.get_text(*buffer.get_bounds(), True), "keys": keys,
                           "focused": window.is_active() and view.has_focus()})

    @dbus.service.method("app.transcribe.TestFixture")
    def reset(self):
        view.get_buffer().set_text("")
        keys.clear()


fixture = Fixture(bus, "/app/transcribe/TestFixture")
window.connect("destroy", Gtk.main_quit)
window.show_all()
view.grab_focus()
print(bus.get_unique_name(), flush=True)
Gtk.main()
