// sshremote helper: lets the sshremote agent list windows and bring one forward, which
// GNOME Shell otherwise allows only its own screen-sharing portal to do, and type text no key of
// the keyboard layout makes (Korean, accented letters), as GNOME's on-screen keyboard does. It
// also holds a program's own window over a pane of the sshremote app (on Wayland no program may
// place another's window): caught as it opens, unseen until it is in place, moved with the app's
// window, minimized while its pane is not shown, and let go when the pane closes or the app ends.

import GLib from 'gi://GLib';
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
    <method name="Commit">
      <arg type="s" direction="in" name="text"/>
      <arg type="b" direction="out" name="done"/>
    </method>
    <method name="Catch">
      <arg type="u" direction="in" name="pid"/>
      <arg type="u" direction="out" name="token"/>
    </method>
    <method name="Caught">
      <arg type="u" direction="in" name="token"/>
      <arg type="t" direction="out" name="id"/>
    </method>
    <method name="Forget">
      <arg type="u" direction="in" name="token"/>
    </method>
    <method name="Hold">
      <arg type="t" direction="in" name="id"/>
      <arg type="u" direction="in" name="pid"/>
      <arg type="i" direction="in" name="width"/>
      <arg type="i" direction="in" name="height"/>
      <arg type="i" direction="in" name="x"/>
      <arg type="i" direction="in" name="y"/>
      <arg type="i" direction="in" name="w"/>
      <arg type="i" direction="in" name="h"/>
      <arg type="b" direction="in" name="shown"/>
      <arg type="s" direction="out" name="state"/>
    </method>
    <method name="Release">
      <arg type="t" direction="in" name="id"/>
      <arg type="b" direction="in" name="close"/>
    </method>
  </interface>
</node>`;

export default class DeskHelper extends Extension {
    enable() {
        this._waits = new Map();
        this._next = 1;
        this._held = new Map();
        this._created = global.display.connect('window-created', (_d, w) => this._onCreated(w));
        this._focused = global.display.connect('notify::focus-window', () => this._keepOver());
        this._object = Gio.DBusExportedObject.wrapJSObject(IFACE, this);
        this._object.export(Gio.DBus.session, PATH);
        this._owner = Gio.bus_own_name_on_connection(Gio.DBus.session, NAME,
            Gio.BusNameOwnerFlags.NONE, null, null);
    }

    disable() {
        Gio.bus_unown_name(this._owner);
        this._object.unexport();
        this._object = null;
        global.display.disconnect(this._created);
        global.display.disconnect(this._focused);
        for (const id of [...this._held.keys()])
            this.Release(id, false);
        for (const wait of this._waits.values())
            this._unhush(this._window(wait.id));
        this._waits.clear();
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

    _window(id) {
        return id ? global.display.list_all_windows().find(x => x.get_id() === id) ?? null : null;
    }

    // Looks out for the next window a program opens (not one of the app's, process `pid`): it is
    // caught as it is made, unseen from its first frame.
    Catch(pid) {
        const now = GLib.get_monotonic_time();
        for (const [token, wait] of this._waits) {
            if (now - wait.since > 60e6)
                this.Forget(token);
        }
        const token = this._next++;
        this._waits.set(token, {pid, id: 0, since: now});
        return token;
    }

    Caught(token) {
        return this._waits.get(token)?.id ?? 0;
    }

    Forget(token) {
        const wait = this._waits.get(token);
        this._waits.delete(token);
        if (wait?.id && !this._held.has(wait.id))
            this._unhush(this._window(wait.id));
    }

    _onCreated(win) {
        if (!this._waits.size || win.get_window_type() !== Meta.WindowType.NORMAL || win.get_transient_for())
            return;
        for (const wait of this._waits.values()) {
            if (wait.id)
                continue;
            if (win.get_pid() === wait.pid)
                return;
            wait.id = win.get_id();
            this._hush(win);
            return;
        }
    }

    // Unseen (its window actor painted with no opacity, whatever its opening animation does),
    // until it is in place.
    _hush(win) {
        const actor = win.get_compositor_private();
        if (actor) {
            actor.set_opacity_override(0);
            return;
        }
        const id = win.connect('shown', () => {
            win.disconnect(id);
            win.get_compositor_private()?.set_opacity_override(0);
        });
    }

    _unhush(win) {
        win?.get_compositor_private()?.set_opacity_override(-1);
    }

    _hushed(win) {
        return win.get_compositor_private()?.get_opacity_override() === 0;
    }

    // The app's window: of process `pid`, `width` by `height` (its pane's coordinates start there).
    _ours(pid, width, height) {
        const near = (a, b) => Math.abs(a - b) <= 3;
        const mine = global.display.list_all_windows().filter(w => w.get_pid() === pid);
        return mine.find(w => {
            const b = w.get_buffer_rect();
            return near(b.width, width) && near(b.height, height);
        }) ?? (mine.length === 1 ? mine[0] : null);
    }

    // Window `id` over the pane at x, y (w by h) of the app's window, or out of sight: the
    // state it is in ("gone", "placing" while unseen, "held", "hidden").
    Hold(id, pid, width, height, x, y, w, h, shown) {
        const win = this._window(id);
        if (!win) {
            this._drop(id);
            return 'gone';
        }
        let held = this._held.get(id);
        if (!held) {
            const caught = [...this._waits.values()].some(wait => wait.id === id) || this._hushed(win);
            held = {win, ours: null, rel: null, shown: false, sigs: [], since: GLib.get_monotonic_time(), there: 0,
                was: caught ? null : win.get_frame_rect()};
            this._held.set(id, held);
        }
        const ours = this._ours(pid, width, height);
        if (ours !== held.ours) {
            this._unfollow(held);
            held.ours = ours;
            if (ours) {
                // The app's window moved or sized (which it is not told of on Wayland), minimized,
                // or closed (it ended): the program's window follows.
                for (const signal of ['position-changed', 'size-changed', 'notify::minimized'])
                    held.sigs.push([ours, ours.connect(signal, () => this._place(held))]);
                held.sigs.push([ours, ours.connect('unmanaged', () => this.Release(id, false))]);
            }
        }
        held.rel = {x, y, w, h};
        held.shown = shown;
        this._place(held);
        if (this._hushed(win))
            return 'placing';
        return held.shown && ours && !ours.minimized ? 'held' : 'hidden';
    }

    _place(held) {
        const {win, ours, rel} = held;
        if (!ours || !rel || !this._window(win.get_id()))
            return;
        const actor = win.get_compositor_private();
        if (!held.shown || ours.minimized) {
            if (!win.minimized) {
                if (win.has_focus())
                    ours.activate(global.get_current_time());
                if (actor)
                    Main.wm.skipNextEffect(actor);
                win.minimize();
            }
            return;
        }
        if (win.minimized) {
            if (actor)
                Main.wm.skipNextEffect(actor);
            win.unminimize();
        }
        const maximized = win.is_maximized ? win.is_maximized() : win.get_maximized() !== 0;
        if (maximized) {
            if (win.set_unmaximize_flags)
                win.unmaximize();
            else
                win.unmaximize(Meta.MaximizeFlags.BOTH);
        }
        if (win.is_fullscreen())
            win.unmake_fullscreen();
        const b = ours.get_buffer_rect();
        const to = {x: b.x + rel.x, y: b.y + rel.y, width: rel.w, height: rel.h};
        const f = win.get_frame_rect();
        if (f.x !== to.x || f.y !== to.y || f.width !== to.width || f.height !== to.height)
            win.move_resize_frame(false, to.x, to.y, to.width, to.height);
        this._keepOver();
        // Seen once it is in place and has had a moment to draw itself there, or after a while.
        if (this._hushed(win)) {
            const now = GLib.get_monotonic_time();
            const there = Math.abs(f.x - to.x) <= 1 && Math.abs(f.y - to.y) <= 1;
            held.there = there ? held.there || now : 0;
            if ((there && now - held.there > 80e3) || now - held.since > 3e6)
                this._unhush(win);
        }
    }

    // Over the app's window while that is the one in use (it comes up over the program's when
    // clicked), not over whatever else is over it.
    _keepOver() {
        const focus = global.display.get_focus_window();
        for (const held of this._held.values()) {
            if (!held.ours || held.ours !== focus || !held.shown || held.win.minimized)
                continue;
            const [, top] = global.display.sort_windows_by_stacking([held.win, held.ours]);
            if (top === held.ours)
                held.win.raise();
        }
    }

    _unfollow(held) {
        for (const [w, sig] of held.sigs)
            w.disconnect(sig);
        held.sigs = [];
    }

    _drop(id) {
        const held = this._held.get(id);
        if (held)
            this._unfollow(held);
        this._held.delete(id);
        for (const [token, wait] of this._waits) {
            if (wait.id === id)
                this._waits.delete(token);
        }
    }

    // Let go: seen, shown, its own again; asked to close (it may ask to save first), or put
    // back where it was if it was open before.
    Release(id, close) {
        const held = this._held.get(id);
        const win = this._window(id);
        this._drop(id);
        if (!win)
            return;
        this._unhush(win);
        if (win.minimized) {
            const actor = win.get_compositor_private();
            if (actor)
                Main.wm.skipNextEffect(actor);
            win.unminimize();
        }
        if (close)
            win.delete(global.get_current_time());
        else if (held?.was)
            win.move_resize_frame(false, held.was.x, held.was.y, held.was.width, held.was.height);
    }

    // Text into the focused text field through the input method; false when none takes text so.
    Commit(text) {
        if (!Main.inputMethod.currentFocus)
            return false;
        Main.inputMethod.commit(text);
        return true;
    }
}
