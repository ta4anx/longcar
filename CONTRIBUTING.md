# Contributing

Thanks for helping with Longcar. The [wiki](https://github.com/ta4anx/longcar/wiki) is the
user-facing documentation; this file is about the code.

## Ground rules

- **Stock GNOME, stock Sunshine.** No vkms, no Sunshine fork, no Flatpak-managed Sunshine.
  Virtual outputs come from Mutter's private ScreenCast API; capture goes through the
  regular XDG portal.
- **The daemon owns state.** GUI, `longcarctl` and the Shell extension are D-Bus clients of
  `longcar-daemon`; nothing else may create virtual displays or start Sunshine.
- **Failures must be recoverable.** Every start saves a layout snapshot first; every stop
  restores it unless the user changed the layout; Sunshine units are transient and bounded
  on stop.

## Layout

| Crate | Contents |
| --- | --- |
| `longcar-core` | config/model/validation, layout planner, ports, Sunshine config/API/log parsing, state JSON, network helpers |
| `longcar-display` | Mutter ScreenCast + DisplayConfig clients, GStreamer keepalive |
| `longcar-daemon` | orchestrator, Sunshine instances, D-Bus service, doctor |
| `longcar-gui` | GTK4/libadwaita app (`longcar`) |
| `longcar-cli` | `longcarctl` |
| `extension/` | GNOME 50 Quick Settings extension |

## Build and check

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
./scripts/build-extension.sh
./target/debug/longcarctl doctor --self-test   # needs a live GNOME Wayland session
```

The live display test (`cargo test -p longcar-display --test mutter -- --ignored`) creates
and removes a 1280×720 virtual monitor; run it before touching capture code.

## Running a dev session

```bash
./scripts/run-dev.sh      # builds, stops installed units, runs daemon + GUI, cleans up
./scripts/install-dev.sh  # ~/.local install + extension, no sudo
```

Changes that touch the config schema need: a serde default or a `Config::migrate` step, the
state document updated (`ScreenEntry`), the GUI and CLI updated, and a test that loads the
old file.

## Pull requests

- Keep commits focused; describe the user-visible effect.
- New behaviour needs a test where it is testable without a session (config, layout, ports,
  Sunshine rendering, log parsing, state JSON, extension menu structure).
- Explain anything you could not verify (portal approval and real streaming need a human and
  a second device).

## Reporting bugs

Use the issue template and include `longcarctl doctor --json`, `longcarctl status --json`,
and the relevant `sunshine.log` lines. Never paste Sunshine API credentials.
