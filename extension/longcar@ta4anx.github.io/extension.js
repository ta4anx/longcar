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

const BITRATE_PRESETS_KBPS = [4000, 6000, 8000, 10000, 15000, 20000, 30000];

const POSITION_OPTIONS = [
    ['left', 'Left'],
    ['right', 'Right'],
    ['above', 'Above'],
    ['below', 'Below'],
];

const CHECKED = '✓ ';
const UNCHECKED = '   ';

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
        this._screens = [];
        this._presets = [];
        this._warnings = [];
        this._staleDefault = false;
        this._audioInfoPending = false;
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
                if (!isOpen)
                    return;

                this._refresh();
                this._refreshAudioInfo();
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
        this._screens = [];
        this._presets = [];
        this._warnings = [];
        this._staleDefault = false;
        this._audioInfoPending = false;
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
        this._screens = [];
        this._presets = [];
        this._warnings = [];
        this._staleDefault = false;
        this._audioInfoPending = false;
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
            this._screens = [];
            this._presets = [];
            this._warnings = [];
            this._staleDefault = false;
            this._audioInfoPending = false;
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

        this._screens = screens;
        this._presets = presets;
        this._warnings = warnings;

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

        const signature = this._signatureFor(screens, presets, warnings);

        const isOpen = this._toggle.menu.isOpen;
        if (isOpen || signature !== this._signature) {
            this._signature = signature;
            this._rebuildMenu(screens, presets, warnings);
        }

        if (isOpen)
            this._refreshAudioInfo();
    }

    _signatureFor(screens, presets, warnings) {
        return JSON.stringify({
            screens: screens.map(screen => [
                screen.id,
                screen.name,
                screen.state,
                screen.maxBitrateKbps ?? 0,
                Boolean(screen.audio),
                Boolean(screen.touch),
                Boolean(screen.keyboard),
                Boolean(screen.mouse),
                Boolean(screen.controller),
                screen.position ?? '',
                screen.relativeTo ?? '',
                screen.alignment ?? '',
            ]),
            presets: presets.map(preset => [preset.id, preset.name, preset.screens]),
            warnings,
            staleDefault: this._staleDefault,
        });
    }

    _refreshAudioInfo() {
        if (this._audioInfoPending || !this._available)
            return;

        this._audioInfoPending = true;
        this._call('GetAudioInfo', null, (error, json) => {
            this._audioInfoPending = false;

            if (error) {
                this._setStaleDefault(false);
                return;
            }

            let info;
            try {
                info = JSON.parse(json);
            } catch (parseError) {
                console.error(`Longcar: malformed audio JSON: ${parseError}`);
                this._setStaleDefault(false);
                return;
            }

            this._setStaleDefault(Boolean(info?.staleDefault));
        });
    }

    _setStaleDefault(stale) {
        if (this._staleDefault === stale)
            return;

        this._staleDefault = stale;

        if (!this._state || !this._toggle?.menu.isOpen)
            return;

        this._signature =
            this._signatureFor(this._screens, this._presets, this._warnings);
        this._rebuildMenu(this._screens, this._presets, this._warnings);
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
            const item = this._buildScreenMenu(screen);
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

        if (this._staleDefault) {
            const restoreAudio = new PopupMenu.PopupMenuItem('Restore host audio');
            restoreAudio.connect('activate', () => this._onRestoreHostAudio());
            menu.addMenuItem(restoreAudio);
            this._menuItems.push(restoreAudio);
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

    _buildScreenMenu(screen) {
        const screenId = screen.id;
        const state = screen.state ?? 'off';
        const active = ACTIVE_STATES.has(state);
        const label = state === 'waitingPortal'
            ? `${screen.name} — approve in GNOME`
            : `${screen.name} — ${state}`;

        const item = new PopupMenu.PopupSubMenuMenuItem(label, false);
        const style = STATE_STYLES[state];
        if (style)
            item.label.add_style_class_name(style);

        const bitrate = screen.maxBitrateKbps ?? 0;
        console.debug(`longcar: screen ${screenId} bitrate ${bitrate}`);

        const width = screen.width ?? 0;
        const height = screen.height ?? 0;
        const rate = bitrate > 0 ? `${bitrate / 1000} Mbps` : 'no cap';

        const modeParts = [`${width}×${height}`, rate];
        if (screen.audio)
            modeParts.push('audio');
        if (screen.touch)
            modeParts.push('touch');

        const inputs = [];
        if (screen.keyboard)
            inputs.push('kbd');
        if (screen.mouse)
            inputs.push('mouse');
        if (screen.controller)
            inputs.push('pad');
        modeParts.push(inputs.length > 0 ? inputs.join('/') : 'no input');

        const mode = new PopupMenu.PopupMenuItem(modeParts.join(' · '),
            {reactive: false, can_focus: false, activate: false});
        mode.setSensitive(false);
        item.menu.addMenuItem(mode);

        const start = new PopupMenu.PopupMenuItem('Start');
        start.setSensitive(!active);
        start.connect('activate', () => this._onStartScreen(screenId));
        item.menu.addMenuItem(start);

        const stop = new PopupMenu.PopupMenuItem('Stop');
        stop.setSensitive(active);
        stop.connect('activate', () => this._onStopScreen(screenId));
        item.menu.addMenuItem(stop);

        item.menu.addMenuItem(new PopupMenu.PopupSeparatorMenuItem());

        const bitrateMenu = new PopupMenu.PopupSubMenuMenuItem('Bitrate', false);
        for (const kbps of BITRATE_PRESETS_KBPS) {
            const checked = bitrate === kbps;
            const preset = new PopupMenu.PopupMenuItem(
                `${checked ? CHECKED : UNCHECKED}${kbps / 1000} Mbps`);
            preset.connect('activate', () => this._onSetBitrate(screenId, kbps));
            bitrateMenu.menu.addMenuItem(preset);
        }

        const noCap = new PopupMenu.PopupMenuItem(
            `${bitrate === 0 ? CHECKED : UNCHECKED}No cap`);
        noCap.connect('activate', () => this._onSetBitrate(screenId, 0));
        bitrateMenu.menu.addMenuItem(noCap);
        item.menu.addMenuItem(bitrateMenu);

        const positionMenu = new PopupMenu.PopupSubMenuMenuItem('Position', false);
        for (const [value, text] of POSITION_OPTIONS) {
            const checked = screen.position === value;
            const option = new PopupMenu.PopupMenuItem(
                `${checked ? CHECKED : UNCHECKED}${text}`);
            option.connect('activate', () => this._onSetLayout(screenId, value));
            positionMenu.menu.addMenuItem(option);
        }
        item.menu.addMenuItem(positionMenu);

        item.menu.addMenuItem(new PopupMenu.PopupSeparatorMenuItem());

        const audioItem = new PopupMenu.PopupMenuItem(
            `${screen.audio ? CHECKED : UNCHECKED}Audio`);
        audioItem.connect('activate',
            () => this._onToggleAudio(screenId, !screen.audio));
        item.menu.addMenuItem(audioItem);

        const touchItem = new PopupMenu.PopupMenuItem(
            `${screen.touch ? CHECKED : UNCHECKED}Touch`);
        touchItem.connect('activate',
            () => this._onToggleTouch(screenId, !screen.touch));
        item.menu.addMenuItem(touchItem);

        const inputMenu = new PopupMenu.PopupSubMenuMenuItem('Input', false);
        const inputOptions = [
            ['keyboard', 'Keyboard', Boolean(screen.keyboard)],
            ['mouse', 'Mouse', Boolean(screen.mouse)],
            ['controller', 'Controller', Boolean(screen.controller)],
        ];
        for (const [kind, text, checked] of inputOptions) {
            const option = new PopupMenu.PopupMenuItem(
                `${checked ? CHECKED : UNCHECKED}${text}`);
            option.connect('activate',
                () => this._onSetInput(screenId, kind, !checked));
            inputMenu.menu.addMenuItem(option);
        }
        item.menu.addMenuItem(inputMenu);

        return item;
    }

    _findScreen(screenId) {
        const screens = Array.isArray(this._state?.screens) ? this._state.screens : [];
        return screens.find(screen => screen.id === screenId) ?? null;
    }

    _onStartScreen(screenId) {
        console.debug(`longcar: start screen ${screenId}`);
        this._activateItem('StartScreen', new GLib.Variant('(s)', [screenId]));
    }

    _onStopScreen(screenId) {
        console.debug(`longcar: stop screen ${screenId}`);
        this._activateItem('StopScreen', new GLib.Variant('(s)', [screenId]));
    }

    _onSetBitrate(screenId, kbps) {
        console.debug(`longcar: set bitrate ${kbps}`);
        this._activateItem('SetScreenBitrate',
            new GLib.Variant('(su)', [screenId, kbps]));
    }

    _onSetLayout(screenId, position) {
        const screen = this._findScreen(screenId);
        const relativeTo = screen?.relativeTo || 'primary';
        const alignment = screen?.alignment || 'center';

        console.debug(`longcar: set layout ${position} ${relativeTo} ${alignment}`);
        this._activateItem('SetScreenLayout',
            new GLib.Variant('(ssss)', [screenId, position, relativeTo, alignment]));
    }

    _onToggleAudio(screenId, enabled) {
        console.debug(`longcar: set audio ${enabled}`);
        this._activateItem('SetScreenAudio',
            new GLib.Variant('(sb)', [screenId, enabled]));
    }

    _onToggleTouch(screenId, enabled) {
        console.debug(`longcar: set touch ${enabled}`);
        this._activateItem('SetScreenTouch',
            new GLib.Variant('(sb)', [screenId, enabled]));
    }

    _onSetInput(screenId, kind, enabled) {
        console.debug(`longcar: set input ${kind} ${enabled}`);
        this._activateItem('SetScreenInput',
            new GLib.Variant('(ssb)', [screenId, kind, enabled]));
    }

    _onRestoreHostAudio() {
        console.debug('longcar: restore host audio');
        this._closeMenu();
        this._call('RestoreHostAudio', null, error => {
            if (error) {
                Main.notify('Longcar', error.message ?? String(error));
                this._refresh();
                return;
            }

            this._staleDefault = false;
            this._refreshAudioInfo();
            this._refresh();
        });
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
