# Longcar troubleshooting

Run `longcarctl doctor` first: it checks the session, Mutter version, portal, Sunshine,
systemd, GStreamer plugins, fractional scaling, layout mode, uinput, ports, layout
snapshot, extension installation and duplicate resolutions. `longcarctl doctor --json`
gives the same data for bug reports, and `longcarctl logs -n 200` adds daemon and
Sunshine log lines with credentials redacted.

## "Sunshine is not installed" / `sunshine-missing`

Longcar manages the **native** Sunshine package; Flatpak is detected and refused.
Install Sunshine 2026.914.0 or newer from
<https://github.com/LizardByte/Sunshine/releases>, then re-run `longcarctl doctor`. If
you keep it outside `PATH`, export `SUNSHINE_BIN=/absolute/path/to/sunshine`.

## Portal approval loops, stale token, `portal-timeout`

Every screen needs a portal restore token, created by approving GNOME's screen picker
once per screen.

- The picker lists monitors by resolution: pick the one whose resolution matches the
  screen you started (`2560x1600` for the default tablet screen).
- If the token becomes stale (the monitor disappeared while Sunshine was stopped), the
  daemon logs `[portalgrab] Portal returned no streams ... discarding the stale token`,
  returns the screen to `waitingPortal`, and shows the approval notification again.
- A 120 second deadline applies; when it expires the screen fails with stage
  `PortalAuth` and is rolled back.
- `longcarctl fix reauthorize:<screen-id>` deletes the token and restarts the screen.

## The requested scale is not offered

Mutter only offers each output's own scale list (for a 2560x1600 virtual monitor: 1.0,
1.25, 1.3333334, 1.6666666, 2.0, 2.5, 2.6666667 — never 1.5). When the requested scale
is more than 0.05 away from every supported value, Longcar uses the closest supported
scale and logs it instead of failing the screen:

```bash
longcarctl logs -n 200 | grep scale
```

The screen's logical size in `longcarctl status --json` reflects the applied scale.

## Sunshine API bootstrap

Longcar creates the Sunshine web credentials itself on first start of each screen:

- `GET /api/config` answering `307` with `Location: /welcome` means no administrator
  exists yet, so Longcar sets one and writes `credentials.json` (mode 0600) *before* the
  handshake.
- `401` afterwards means the credentials no longer match Sunshine's state (for example
  after `sunshine_state.json` was edited). Longcar refuses to guess and reports
  `Sunshine already has API credentials that Longcar does not know`. Recover with
  `longcarctl fix reset-instance:<screen-id>`, which deletes that screen's pairings.
- `[portalgrab] Saved portal restore token` in `sunshine.log` is what moves a screen to
  `ready`. `Portal capture is awaiting user permission` is Sunshine's own `Fatal:` line
  while the picker is open; Longcar treats it as "still waiting", not as a failure.

## The first approval does not produce video

Sunshine initialises its portal capture during startup, before anyone can approve the
picker, and logs `Portal capture is awaiting user permission` plus the KMS fallback
attempt. The portal request stays open (Sunshine holds a session object) until you
answer the picker, and a saved token (`[portalgrab] Saved portal restore token`) is
reused on subsequent roots.

If a client connects right after the very first approval and shows a black screen,
restart that screen so Sunshine initialises capture with the new token:

```bash
longcarctl stop --screen tablet && longcarctl start --screen tablet
```

The token is kept, so no second approval is needed. `longcarctl logs -n 200 | grep
portalgrab` shows whether the token was saved.

## A tablet shows a black screen and the decoder crashes

Older tablets (Galaxy Note/Tab era) crash when the negotiated stream is more than the
decoder supports: HEVC/AV1, a profile/level above the SoC, or Sunshine's **Vulkan** H.264
fallback when VAAPI cannot initialise (no `/dev/dri/renderD*`).

Fix in this order, per screen:

1. `hevc = off`, `av1 = off` — Sunshine then offers H.264 only.
2. `encoder = software` when `sunshine.log` says `Encoder [vaapi] failed`. The log should
   then show `Creating encoder [libx264]` and `profile High, level 4.0, 4:2:0`.
3. Screen mode = the tablet panel (e.g. `1280x800`), client 30 fps, 8–10 Mbps; raise later.
4. Still crashing → 1280x720@30 → 800x600.

## The host speakers went silent after streaming

Sunshine switches the default sink to its own virtual sink (`sink-sunshine-stereo`, …) and
mutes the physical one while it streams. A stream killed mid-flight leaves both behind, so
the desktop keeps playing into the virtual sink you cannot hear.

- `longcarctl audio` shows the host default sink, every sink's volume/mute state, and each
  screen's effective capture sink. A stale Sunshine sink is flagged.
- `longcarctl audio --restore-host` puts the physical sink back (the daemon also does this
  on startup and after the last screen stops, including the volume and mute state it
  snapshotted before the first start).
- The app's **Audio** page has the same "Restore host output" button plus volume and mute
  controls per sink.

If the sink is at 0 % or muted after a restore, set it back with the slider (or
`wpctl set-volume <id> 50%`); Longcar never changes your volume on its own.

## Audio goes to the wrong device

Audio is a per-screen flag (`audio`), not a global. Turn it off on a screen to let that
device use its own speakers; turn it on where you want the desktop mix. Two screens with
audio on receive the same mix. Changing it restarts that screen's unit, so stop the stream
first (or change it while the screen is only `ready`).

## Touch acts like a mouse, or does nothing

`touch` maps to Sunshine's `native_pen_touch`: on = real touchscreen events, off = touches
are translated into pointer input. Apps that ignore touch want it off; GNOME itself wants
it on. The toggle is on each screen row, in Quick Settings, and in the screen dialog.

## Two screens with the same resolution

Sunshine's portal capture picks a monitor by resolution. Two virtual monitors with
identical width/height cannot be distinguished, so the configuration is rejected
(`duplicate resolution`). Change one screen's resolution in the GUI or with
`longcarctl screens update`.

## Port already in use

Each screen owns a 60-port family (`ss -ltnup 'sport = :48049'` shows the offender).
Common causes: a manually started Sunshine, an old `longcar-sunshine-*` unit, or
another screen in the same slot. Use a free slot, or stop the other process.

## Layout is not restored / `layout-restore-available`

The snapshot is kept instead of applied when the layout changed while screens were
active, when the restore failed, or when `restore_layout_on_stop` is off.

- `longcarctl restore-layout` applies it (requires every screen stopped).
- `longcarctl discard-layout` deletes it.

## GNOME's "Stop Sharing" indicator killed my screen

Do not press the *Stop Sharing* indicator for Longcar's virtual outputs: it closes the
Mutter ScreenCast session, which removes the virtual monitor and the Sunshine capture.
Longcar notices the session closing, stops that screen's Sunshine unit, marks the
screen `error` with stage `MutterSession` and warning `session-closed`, and keeps the
layout snapshot. Use Longcar or the Quick Settings toggle to stop instead.

## The Quick Settings toggle is missing

The extension only loads at login on Wayland.

```bash
longcarctl setup                 # install + enable extension and service
gnome-extensions list | grep longcar
```

Log out and back in once, then check the top panel. `longcarctl doctor` reports the
`extension` check; `longcarctl fix install-extension` reinstalls it.

## Daemon unavailable

```bash
systemctl --user status longcar-daemon.service
journalctl --user -u longcar-daemon.service -n 100 --no-pager
systemctl --user start longcar-daemon.service
```

The GUI shows a status page with this command when the bus name is not owned. The CLI
exits with code 2 when it cannot reach the daemon.

## Nothing renders / screen goes straight to `error`

Check `pipewiresrc` availability (`gst-inspect-1.0 pipewiresrc`) and
`longcarctl doctor --self-test`, which creates a temporary 1366x768 virtual monitor and
removes it again. Errors map to stages: `RecordVirtual` and `PipeWireNode` are Mutter
and GStreamer problems, `Layout` means the requested placement could not be applied.

## Input does not reach my desktop

Sunshine injects input through uinput. `/dev/uinput` must be read/writable for your
user (`doctor` warns when it is not). Multiple streams share one GNOME seat: the
pointer is the same on every screen, so do not drive two screens at once.

## Audio plays on the wrong screen

Audio is a per-screen flag (`audio` in `<screen>`), exposed as a switch on each Screens
row, in Settings, and in the Quick Settings menu. Turn it off on a device to let it use
its own speakers. `longcarctl audio off` disables audio everywhere, `longcarctl audio
<screen>` routes it to exactly one screen:

```bash
longcarctl audio off
longcarctl audio samsung-tab
```

Active screens are restarted to apply it, so stop a live stream first.

## Suspend and resume

After resume, GNOME may have re-created outputs and Sunshine's PipeWire node may be
gone. Expected recovery: the screen goes to `error`, `longcarctl status` shows it, and
`longcarctl start --screen <id>` prepares a fresh display. A new portal approval is
normally not required because the token is stored per instance.

## Fractional scaling is unavailable

The virtual output needs Mutter's `scale-monitor-framebuffer` experimental feature:
`longcarctl fix enable-fractional-scaling` appends it while preserving existing
features, then log out and back in.

## Collecting diagnostics

```bash
longcarctl doctor --json > /tmp/longcar-doctor.json
longcarctl logs -n 300 > /tmp/longcar-logs.txt
gdbus call --session --dest org.gnome.Mutter.DisplayConfig \
  --object-path /org/gnome/Mutter/DisplayConfig \
  --method org.gnome.Mutter.DisplayConfig.GetCurrentState > /tmp/longcar-display.txt
```

Settings → Diagnostics has *Copy diagnostics*, and Activity has *Save diagnostics*,
which writes a redacted report.
