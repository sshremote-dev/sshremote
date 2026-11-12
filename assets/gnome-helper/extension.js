// sshremote helper: lets the sshremote agent list windows and bring one forward, which
// GNOME Shell otherwise allows only its own screen-sharing portal to do.

import Gio from 'gi://Gio';
import Meta from 'gi://Meta';
import Shell from 'gi://Shell';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';

const NAME = 'io.github.thinkbigx.SshRemote.Windows';
const PATH = '/io/github/thinkbigx/SshRemote/Windows';
const IFACE = `
<node>
  <interface name="${NAME}">
    <method name="List">
      <arg type="a(tssiiiibbbt)" direction="out" name="windows"/>
    </method>
    <method name="Activate">
      <arg type="t" direction="in" name="id"/>
    </method>
  </interface>
</node>`;

export default class DeskHelper extends Extension {
    enable() {
        this._object = Gio.DBusExportedObject.wrapJSObject(IFACE, this);
        this._object.export(Gio.DBus.session, PATH);
        this._owner = Gio.bus_own_name_on_connection(Gio.DBus.session, NAME,
            Gio.BusNameOwnerFlags.NONE, null, null);
    }

    disable() {
        Gio.bus_unown_name(this._owner);
        this._object.unexport();
        this._object = null;
    }

    // Every window from the top of the stack down: id, title, program, frame in stage
    // coordinates, hidden (minimized, or on a workspace not shown, where its frame still lies over
    // the screen), shareable on its own, focused, and the window it belongs to.
    List() {
        const tracker = Shell.WindowTracker.get_default();
        const focus = global.display.get_focus_window();
        const shown = global.workspace_manager.get_active_workspace();
        const windows = global.display.sort_windows_by_stacking(global.display.list_all_windows()).reverse();
        return windows.map(w => {
            const r = w.get_frame_rect();
            const app = tracker.get_window_app(w);
            let root = w;
            while (root.get_transient_for())
                root = root.get_transient_for();
            const types = [Meta.WindowType.NORMAL, Meta.WindowType.DIALOG, Meta.WindowType.MODAL_DIALOG,
                Meta.WindowType.UTILITY];
            const normal = root === w && !w.is_skip_taskbar() && types.includes(w.get_window_type());
            return [w.get_id(), w.get_title() ?? '', app ? app.get_name() : (w.get_wm_class() ?? ''),
                r.x, r.y, r.width, r.height, w.minimized || !w.located_on_workspace(shown), normal, w === focus,
                root.get_id()];
        });
    }

    Activate(id) {
        const w = global.display.list_all_windows().find(x => x.get_id() === id);
        if (w)
            Main.activateWindow(w);
    }
}
