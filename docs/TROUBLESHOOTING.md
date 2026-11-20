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

Only the screen named by `general.audio_screen` enables `stream_audio`. Change it in the
app's Settings page (allowed only while no screen is streaming):
`longcarctl` has no audio subcommand, but the same settings call works from D-Bus:

```bash
gdbus call --session --dest io.github.ta4anx.Longcar.Daemon \
  --object-path /io/github/ta4anx/Longcar/Daemon \
  --method io.github.ta4anx.Longcar.Daemon.Control.SetAudioScreen tablet
```

Active screens are restarted to apply it.

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

The GUI's Doctor page has *Copy diagnostics*, and Activity has *Save diagnostics*,
which writes a redacted report.
