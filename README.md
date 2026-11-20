# Longcar

Virtual displays for Sunshine and Moonlight, for GNOME 50 on Wayland.

Longcar turns a tablet or phone into an extra GNOME desktop screen. It creates a real
virtual monitor with Mutter, places it next to your physical monitors, and streams it to
a Moonlight (tablet, landscape) or Artemis (phone, portrait) client. Manage screens,
presets, pairing, codec/bitrate caps and recovery from the app, `longcarctl`, or a Quick
Settings toggle in the GNOME top panel.

![Longcar screens page](docs/screenshots/main.png)

**This extends the GNOME desktop, not Android's display topology.** The tablet is a
fullscreen stream of a GNOME virtual monitor; it is not a second Android display.

## Requirements

- GNOME 50 on Wayland (Mutter ScreenCast API v4, `layout-mode == 1`).
- Fractional scaling enabled (`scale-monitor-framebuffer` experimental feature) — the
  virtual output is usually scaled.
- Native Sunshine **2026.914.0 or newer**. The Flatpak build is detected but never
  managed, because Longcar needs per-screen process isolation.
- Stock Moonlight for a tablet screen; **Artemis** for a phone in portrait (stock
  Moonlight locks phones to landscape).
- `systemd --user` with `systemd-run` — one transient unit per screen.

## Install

```bash
sudo apt install ./longcar_0.1.0_amd64.deb
longcarctl setup            # installs the Shell extension and enables the service
```

Then **log out and back in once** on Wayland: the Quick Settings indicator is only
created at Shell startup.

Install Sunshine (native, not Flatpak):

```bash
# latest release: https://github.com/LizardByte/Sunshine/releases
sudo apt install ./sunshine-*.deb
longcarctl doctor
```

Build from source instead:

```bash
cargo build --release --workspace
./scripts/install-dev.sh    # installs into ~/.local and the Shell extension
./scripts/run-dev.sh        # runs the daemon and GUI from the source tree
```

## First run

1. `longcarctl doctor` — fix anything reported (`longcarctl fix <id>`).
2. Start the default preset: `longcarctl start` or the header button in the app.
3. GNOME shows a screen picker for the new Sunshine instance. **Pick the monitor whose
   resolution matches the screen** (for the default tablet screen: `2560x1600`). Approve
   once per screen; the restore token is then reused.
4. `longcarctl status --json` reaches `ready` and the virtual connector appears next to
   your monitors.
5. On the tablet, add a host: the address from the app's *Devices → Connection info*
   (for example `192.168.2.11:48049`), then request a PIN.
6. Approve it: `longcarctl devices pending tablet`, then
   `longcarctl devices pair tablet <pairing-id> <pin>`.
7. Connect. The screen becomes `streaming`, and Activity shows bitrate and fps.

Phone in portrait: use *Add phone screen* in the Quick Settings menu (or
`longcarctl`), then connect with Artemis.

## Day to day

```bash
longcarctl status                 # daemon and screen summary
longcarctl start --preset tablet-phone
longcarctl stop --screen phone
longcarctl screens list --json
longcarctl devices list tablet
longcarctl network hosts          # nearby Sunshine hosts
longcarctl doctor --self-test     # creates and removes a temporary virtual display
longcarctl logs -n 200
longcarctl restore-layout
```

Presets start the screens they list and stop the screens they do not. The Quick Settings
toggle starts the default preset and stops everything; its menu lists screens, presets,
*Add phone screen*, and *Restore layout* when a saved layout is pending.

## How it works

Each screen gets its own Mutter ScreenCast session, its own PipeWire node (kept alive by
a local `fakesink`), its own transient Sunshine unit with a private `XDG_CONFIG_HOME`,
and its own port family starting at `48049 + 60 * slot`.

Start order is virtual display → layout applied → Sunshine unit → API credentials →
portal capture authorization; stop runs in reverse and restores your previous monitor
layout. See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the state machine, port
table and file layout, and [docs/TROUBLESHOOTING.md](docs/TROUBLESHOOTING.md) for
failures.

## Limitations

- One Sunshine process per screen: one portal approval per screen, and roughly 1–2
  processes' worth of extra memory per screen.
- Screens must use **unique resolutions**; the portal picker identifies monitors by
  resolution.
- Changing resolution, refresh rate, scale or cursor mode requires a new portal
  approval.
- Sunshine initialises portal capture during startup, before you can answer the picker.
  If a client shows a black screen right after the very first approval, restart that
  screen once (`longcarctl stop --screen tablet && longcarctl start --screen tablet`);
  the saved token is reused and no second approval is needed.
- The requested scale is clamped to the closest scale the virtual output reports (Mutter
  never offers 1.5 for a 2560x1600 output, for example).
- GNOME shares one input seat: every stream controls the same pointer and keyboard.
  Two people cannot use two screens at once, and uinput access is required.
- Only one screen can carry audio (`audio_screen`).
- No HDR. HEVC/AV1 Main10 is a codec advertisement, not an HDR transport guarantee.
- Idle Moonlight clients do not advertise over mDNS; discovery shows Sunshine hosts and
  pending pairings, not sleeping clients.
- Flatpak Sunshine is not managed.
- Validated on AMD VAAPI only; NVIDIA and Intel encoders are untested.
- Bitrate is capped with Sunshine's `max_bitrate`; a client may ask for less, and
  `max_stream_resolution` only downscales.

## Security

Longcar listens on your LAN only: Sunshine is bound to the machine's addresses, and the
daemon has no network listener at all (D-Bus session bus only).

- Do not forward the Sunshine ports from the internet. Pair only devices you trust:
  paired clients get keyboard and mouse control of your desktop.
- Sunshine API credentials are generated per screen instance, stored `0600`, and never
  logged.
- Portal restore tokens are per instance and `0600`.
- The GUI never deletes pairings implicitly; resetting an instance requires
  confirmation (`longcarctl` needs `--yes`).
- `longcarctl network ports` prints the exact `ufw` rules for one screen's port family if
  you need a firewall rule.

## Uninstall

```bash
sudo apt remove longcar
gnome-extensions uninstall longcar@ta4anx.github.io
rm -rf ~/.config/longcar ~/.local/state/longcar
```

## Credits

- [lirenzzzin/gnome-wayland-virtual-monitors-sunshine](https://github.com/lirenzzzin/gnome-wayland-virtual-monitors-sunshine)
  (Unlicense) — the Mutter `RecordVirtual` + portal capture approach this project builds
  on.
- [Sunshine](https://github.com/LizardByte/Sunshine) (GPL-3.0) — the streaming host.
- [Moonlight](https://github.com/moonlight-stream/moonlight-qt) and
  [Artemis](https://github.com/ClassicOldSong/moonlight-android) — clients.
- GNOME Shell, Mutter, GTK and libadwaita.

Longcar is licensed GPL-3.0-or-later.
