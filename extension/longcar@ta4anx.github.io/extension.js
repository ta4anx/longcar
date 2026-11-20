// SPDX-License-Identifier: GPL-3.0-or-later

import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import GObject from 'gi://GObject';
import Shell from 'gi://Shell';

import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import * as PopupMenu from 'resource:///org/gnome/shell/ui/popupMenu.js';
import * as QuickSettings from 'resource:///org/gnome/shell/ui/quickSettings.js';
import * as Util from 'resource:///org/gnome/shell/misc/util.js';

const BUS_NAME = 'io.github.ta4anx.Longcar.Daemon';
const OBJECT_PATH = '/io/github/ta4anx/Longcar/Daemon';
const INTERFACE = 'io.github.ta4anx.Longcar.Daemon.Control';

const APP_DESKTOP_ID = 'io.github.ta4anx.Longcar.desktop';
const APP_BINARY = 'longcar';

const ACTIVE_STATES = new Set([
    'starting',
    'waitingPortal',
    'ready',
    'streaming',
]);

const STATE_STYLES = {
    error: 'longcar-state-error',
    waitingPortal: 'longcar-state-waiting',
    starting: 'longcar-state-pending',
    preparing: 'longcar-state-pending',
    stopping: 'longcar-state-pending',
    ready: 'longcar-state-ready',
    streaming: 'longcar-state-live',
};

const LongcarIndicator = GObject.registerClass(
class LongcarIndicator extends QuickSettings.SystemIndicator {
    _init() {
        super._init();

        this.toggle = new QuickSettings.QuickMenuToggle({
            title: 'Longcar',
            iconName: 'video-display-symbolic',
            toggleMode: true,
        });
        this.quickSettingsItems.push(this.toggle);
    }
});

export default class LongcarExtension extends Extension {
    enable() {
        this._available = false;
        this._syncing = false;
        this._signalId = 0;
        this._nameWatchId = 0;
        this._checkedId = 0;
        this._openStateId = 0;
        this._destroyId = 0;
        this._menuItems = [];
        this._signature = null;
        this._state = null;
        this._active = 0;
        this._live = 0;

        this._indicator = new LongcarIndicator();
        this._toggle = this._indicator.toggle;
        this._toggle.menu.setHeader('video-display-symbolic', 'Longcar');

        Main.panel.statusArea.quickSettings.addExternalIndicator(this._indicator);

        this._checkedId = this._toggle.connect('notify::checked',
            this._onCheckedChanged.bind(this));
        this._destroyId = this._toggle.connect('destroy',
            this._onToggleDestroyed.bind(this));
        this._openStateId = this._toggle.menu.connect('open-state-changed',
            (menu, isOpen) => {
                if (isOpen)
                    this._refresh();
            });

        this._setAvailable(false);

        this._nameWatchId = Gio.bus_watch_name(
            Gio.BusType.SESSION,
            BUS_NAME,
            Gio.BusNameWatcherFlags.NONE,
            this._onNameAppeared.bind(this),
            this._onNameVanished.bind(this));

        this._refresh();
    }

    disable() {
        if (this._nameWatchId) {
            Gio.bus_unwatch_name(this._nameWatchId);
            this._nameWatchId = 0;
        }

        this._unsubscribeSignal();

        if (this._toggle) {
            if (this._checkedId) {
                this._toggle.disconnect(this._checkedId);
                this._checkedId = 0;
            }
            if (this._openStateId) {
                this._toggle.menu.disconnect(this._openStateId);
                this._openStateId = 0;
            }
            if (this._destroyId) {
                this._toggle.disconnect(this._destroyId);
                this._destroyId = 0;
            }
        }

        this._clearMenuItems();

        if (this._indicator) {
            this._indicator.quickSettingsItems.forEach(item => item.destroy());
            this._indicator.quickSettingsItems = [];
            this._indicator.destroy();
        }

        this._indicator = null;
        this._toggle = null;
        this._state = null;
        this._signature = null;
        this._menuItems = [];
        this._available = false;
        this._syncing = false;
    }

    _onNameAppeared() {
        this._setAvailable(true);
        this._subscribeSignal();
        this._refresh();
    }

    _onNameVanished() {
        this._setAvailable(false);
    }

    _onToggleDestroyed() {
        this._destroyId = 0;
        this._toggle = null;
        this._available = false;
        this._state = null;
        this._signature = null;
        this._menuItems = [];
    }

    _subscribeSignal() {
        this._unsubscribeSignal();

        if (!Gio.DBus.session)
            return;

        this._signalId = Gio.DBus.session.signal_subscribe(
            BUS_NAME,
            INTERFACE,
            'StateChanged',
            OBJECT_PATH,
            null,
            Gio.DBusSignalFlags.NONE,
            (connection, sender, path, iface, signal, parameters) => {
                try {
                    const [json] = parameters.deep_unpack();
                    this._applyStateJson(json);
                } catch (error) {
                    console.error(`Longcar: invalid StateChanged payload: ${error}`);
                }
            });
    }

    _unsubscribeSignal() {
        if (!this._signalId)
            return;

        if (Gio.DBus.session) {
            try {
                Gio.DBus.session.signal_unsubscribe(this._signalId);
            } catch (error) {
                console.error(`Longcar: failed to unsubscribe: ${error}`);
            }
        }
        this._signalId = 0;
    }

    _setAvailable(available) {
        this._available = available;

        if (!this._toggle)
            return;

        this._toggle.reactive = available;
        this._toggle.can_focus = available;
        this._toggle.menuEnabled = available;

        if (!available) {
            this._syncing = true;
            try {
                this._toggle.checked = false;
            } finally {
                this._syncing = false;
            }
            this._toggle.subtitle = 'Daemon unavailable';
            this._state = null;
            this._signature = null;
            this._clearMenuItems();
        }
    }

    _onCheckedChanged() {
        if (this._syncing || !this._available)
            return;

        const start = this._toggle.checked;
        const method = start ? 'StartDefault' : 'StopAll';

        this._call(method, null, error => {
            if (!error)
                return;

            Main.notify('Longcar', error.message ?? String(error));
            this._refresh();
        });
    }

    _refresh() {
        this._call('GetState', null, (error, json) => {
            if (error) {
                if (this._isGoneError(error))
                    this._setAvailable(false);
                return;
            }

            this._applyStateJson(json);
        });
    }

    _isGoneError(error) {
        if (!(error instanceof Gio.DBusError))
            return false;

        try {
            const name = Gio.DBusError.get_remote_error(error);
            return name === 'org.freedesktop.DBus.Error.ServiceUnknown' ||
                name === 'org.freedesktop.DBus.Error.NameHasNoOwner';
        } catch (_e) {
            return false;
        }
    }

    _applyStateJson(json) {
        let state;
        try {
            state = JSON.parse(json);
        } catch (error) {
            console.error(`Longcar: malformed state JSON: ${error}`);
            return;
        }

        if (!state || state.schemaVersion !== 1)
            return;

        this._state = state;

        const screens = Array.isArray(state.screens) ? state.screens : [];
        const presets = Array.isArray(state.presets) ? state.presets : [];
        const warnings = Array.isArray(state.warnings) ? state.warnings : [];

        const active = screens.filter(screen =>
            ACTIVE_STATES.has(screen.state)).length;
        const live = screens.filter(screen => screen.state === 'streaming').length;

        this._active = active;
        this._live = live;

        this._syncing = true;
        try {
            this._toggle.checked = active > 0;
        } finally {
            this._syncing = false;
        }

        if (!this._available)
            return;

        this._toggle.subtitle = `${active} active · ${live} live`;

        const signature = JSON.stringify({
            screens: screens.map(screen => [screen.id, screen.name, screen.state]),
            presets: presets.map(preset => [preset.id, preset.name, preset.screens]),
            warnings,
        });

        const isOpen = this._toggle.menu.isOpen;
        if (isOpen || signature !== this._signature) {
            this._signature = signature;
            this._rebuildMenu(screens, presets, warnings);
        }
    }

    _rebuildMenu(screens, presets, warnings) {
        this._clearMenuItems();

        if (!this._toggle)
            return;

        const menu = this._toggle.menu;

        const header = new PopupMenu.PopupMenuItem(
            `${this._active} active · ${this._live} live`,
            {reactive: false, can_focus: false, activate: false});
        header.add_style_class_name('longcar-menu-header');
        header.setSensitive(false);
        menu.addMenuItem(header);
        this._menuItems.push(header);

        for (const screen of screens) {
            const state = screen.state ?? 'off';
            const label = state === 'waitingPortal'
                ? `${screen.name} — approve in GNOME`
                : `${screen.name} — ${state}`;

            const item = new PopupMenu.PopupMenuItem(label,
                {reactive: false, can_focus: false, activate: false});
            const style = STATE_STYLES[state];
            if (style)
                item.label.add_style_class_name(style);
            item.setSensitive(false);
            menu.addMenuItem(item);
            this._menuItems.push(item);
        }

        for (const preset of presets) {
            const item = new PopupMenu.PopupMenuItem(`Start ${preset.name}`);
            item.connect('activate', () => {
                this._activateItem('ApplyPreset',
                    new GLib.Variant('(s)', [preset.id]));
            });
            menu.addMenuItem(item);
            this._menuItems.push(item);
        }

        const addPhone = new PopupMenu.PopupMenuItem('Add phone screen');
        addPhone.connect('activate', () => {
            this._activateItem('AddQuickScreen',
                new GLib.Variant('(s)', ['phone_portrait']));
        });
        menu.addMenuItem(addPhone);
        this._menuItems.push(addPhone);

        if (warnings.includes('layout-restore-available')) {
            const restore = new PopupMenu.PopupMenuItem('Restore layout');
            restore.connect('activate', () => this._activateItem('RestoreLayout', null));
            menu.addMenuItem(restore);
            this._menuItems.push(restore);
        }

        const separator = new PopupMenu.PopupSeparatorMenuItem();
        menu.addMenuItem(separator);
        this._menuItems.push(separator);

        const open = new PopupMenu.PopupMenuItem('Open Longcar');
        open.connect('activate', () => {
            this._closeMenu();
            const app = Shell.AppSystem.get_default().lookup_app(APP_DESKTOP_ID);
            if (app)
                app.activate();
            else
                Util.spawn([APP_BINARY]);
        });
        menu.addMenuItem(open);
        this._menuItems.push(open);
    }

    _clearMenuItems() {
        for (const item of this._menuItems) {
            try {
                item.destroy();
            } catch (error) {
                console.error(`Longcar: failed to destroy menu item: ${error}`);
            }
        }
        this._menuItems = [];
    }

    _activateItem(method, params) {
        this._closeMenu();
        this._call(method, params, error => {
            if (!error)
                return;

            Main.notify('Longcar', error.message ?? String(error));
            this._refresh();
        });
    }

    _closeMenu() {
        if (this._toggle?.menu?.isOpen)
            this._toggle.menu.close();
    }

    _call(method, params, callback) {
        const connection = Gio.DBus.session;
        if (!connection) {
            callback(new Error('No session bus'), null);
            return;
        }

        connection.call(
            BUS_NAME,
            OBJECT_PATH,
            INTERFACE,
            method,
            params,
            null,
            Gio.DBusCallFlags.NONE,
            -1,
            null,
            (source, result) => {
                try {
                    const reply = source.call_finish(result);
                    const unpacked = reply.deep_unpack();
                    callback(null, unpacked.length > 0 ? unpacked[0] : null);
                } catch (error) {
                    callback(error, null);
                }
            });
    }
}
