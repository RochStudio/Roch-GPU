# GUI port notes (WPF `GpuTuner.App` → eframe/egui)

Handoff file for the GUI port. Part 1 = app shell, theme, tray, view model, main window, XOC and
Graphics windows. Part 2 = Monitor, Fan and V/F Curve windows. Both parts are done.

Stack: eframe/egui **0.37** with the **glow** renderer (`default-features = false`, features
`glow`, `default_fonts`, `clipboard`), `tray-icon` 0.26, `image` 0.25 (png only, already pulled in by
eframe). eframe 0.37's `App` trait is `logic(ctx, frame)` + `ui(ui, frame)` (no `update`).

## Running / testing

* `CARGO_TARGET_DIR=target/gui cargo run -p roch-gpu -- --mock` — simulated GPU.
* `--mock` **skips the UAC relaunch** (`main.rs::wants_elevation`): the simulated GPU writes nothing,
  so the GUI can be run from an ordinary shell. Every other GUI launch still elevates (as WPF did).
* Isolate test runs from the real profiles: set `APPDATA` to a scratch folder before launching
  (the store lives in `%APPDATA%\Roch GPU`). Do this — a mock run's Save writes "Slot N" profiles.
* **Do not toggle Startup or the startup-profile combo in a manual test** on a machine with a real
  logon task: the checkbox reflects the *real* Task Scheduler entry (`schtasks /Query`), and
  toggling re-registers/unregisters it (faithful WPF behaviour).
* `cargo test -p roch-gpu` — view-model tests in `gui/tests.rs` (mock backend, temp store, fake
  logon task), plus unit tests in `format.rs`, `assets.rs`, `startup.rs`, `windows/graphics.rs`,
  `telemetry_table.rs`, `fan_editor.rs`, `vf_editor.rs`, `windows/fan.rs`, `windows/monitor.rs`.
* Visual checks: launch with `--mock` and `APPDATA` pointed at a scratch folder (so real profiles and
  settings are untouched), then screenshot the window (e.g. PowerShell `CopyFromScreen`).

## Module map (`src/gui/`)

| file | C# origin | what |
|---|---|---|
| `mod.rs` | `App.OnStartup/OnExit`, `EntryPoint` window half | `run(args)`: store, settings, single-instance claim, panic log, `startup::start`, show-request listener, `eframe::run_native` |
| `startup.rs` | `App.OnStartup` | `Flags::parse`, backend detection + "start simulated?" offer, recovery journal (recover BEFORE the logon profile), `--apply-profile`, `--exit` |
| `shared.rs` | `App` statics | `Shared` (Arc service, store, `Mutex<AppSettings>`, `Mutex<Option<ProfileRecovery>>`, recovery notice, `StartupTasks`), `trial_profile` (= `App.TrialProfile`), `log`/`log_line` (1 MB rotating log) |
| `view_model.rs` | `MainViewModel`, `ProfileSlot` | all UI logic, toolkit-free |
| `app.rs` | `MainWindow.xaml.cs` | `RochApp`: worker thread, tray, hide-to-tray, child windows, modal dialogs, poll background mode, theme toggle, close-with-fan prompt |
| `windows/main_window.rs` | `MainWindow.xaml` | draws the root viewport, returns `MainAction`s |
| `windows/xoc.rs` | `XocWindow` | done |
| `windows/graphics.rs` | `GraphicsWindow` | done |
| `windows/monitor.rs` | `MonitorWindow.xaml(.cs)` | summary cards, sensor table, Open at launch switch, Reset statistics |
| `windows/fan.rs` | `FanWindow.xaml(.cs)` | modes, per-fan sliders, Zero RPM, curve editor painting + pointer handling, point rows, Apply |
| `windows/curve.rs` | `CurveWindow.xaml(.cs)` | toolbar (flatten / remove / reload / reset / apply), V/F editor painting + pointer/keys |
| `telemetry_table.rs` | `ViewModels/TelemetryTable.cs` | rows, grouping, late discovery, formatting — UI-free, tests ported from `GpuTuner.Telemetry.Tests` |
| `fan_editor.rs` | `Controls/FanCurveEditor.cs` (logic half) | point math, hit-testing, drag/add/remove, typed edits — UI-free, tested |
| `vf_editor.rs` | `Controls/VfCurveEditor.cs` (logic half) | axes, hit-testing, snapping, Ctrl/Shift drags, cap/ceiling, keys, status texts — UI-free, tested |
| `windows/mod.rs` | — | `WinCtx`, `ChildChrome`, `child_builder`, `Placement`, `content_frame` |
| `theme.rs` | `Native/Theme.cs`, `App.xaml` brushes | `Palette::{LIGHT,DARK}`, `pal(ctx)`, `apply(ctx, dark)`, `install_fonts`, `bold()/regular()`, `BODY`=12 |
| `widgets.rs` | `App.xaml` styles, `RangeSlider`, `ValueBoxBehavior`, `ThemeGlyph` | `button(ButtonKind)`, `title_button`, `paint_moon`, `layout_slider`/`tight_slider`/`nudged_slider`, `nudge_button` (hold-repeat 400/60 ms), `range_slider`, `value_box`, `checkbox`, `info_line`, `layout_panel`, `label` |
| `format.rs` | .NET format strings | `signed` (`+#;-#;0`), `one_decimal` (`0.#`), `parse_signed` (`TryParseSigned`), `info_line_parts` |
| `tray.rs` | `SetupTray` | Show / Reset GPU to defaults / Exit, double-click = Show; 16 px frame from the .ico |
| `assets.rs` | resources | `icon.png` + `RochGPU.ico` embedded (`crates/roch-gpu/assets/`) |
| `native.rs` | MessageBox, `Process.Start(UseShellExecute)` | `message_box` (pre-window prompts), `shell_open` (links, log) |

## Windows / viewports

* Main window = root viewport, undecorated (custom 30 px title strip: icon, "Roch GPU 1.1.0",
  theme ☀/moon, −, ×), width 660, not resizable; height follows content with a 640 floor
  (`SizeToContent`), via `ViewportCommand::InnerSize` from `RochApp::ui`.
* Every other window is an **immediate viewport** owned by `RochApp` as `Option<T>` (`None` =
  closed). Each frame `RochApp::show_children` builds a `WinCtx { vm: &mut MainViewModel, shared,
  icon, dark, main_rect }` and calls `T::show(&mut self, ctx, &mut WinCtx) -> bool` (false = the user
  closed it). Inside, call `self.chrome.frame(ui.ctx(), w.dark)` first: it sends the title-bar theme
  (`ViewportCommand::SetTheme`, i.e. the DWM dark title bar) on open and on every switch, handles a
  pending focus request and reports close. `ChildChrome::fit(ctx, size)` = `SizeToContent`.
* Clicking a window's button again focuses it (`T::focus()`); Telemetry toggles (focus, or close
  when already focused), as in WPF.
* Placement: `Placement::RightOfOwner` (Monitor, Graphics: `Left = main.Right + 8`), `CenterOwner`
  (XOC, Fan, Curve). Build the viewport with `self.chrome.builder(title, size, icon, placement,
  main_rect)`: the position is worked out once, on open (part 2 fix — recomputing it from the main
  window every frame dragged every child along whenever the main window grew a line).
* `windows::Tap<T>`: a window's own telemetry subscription. `Tap::listen(svc, ctx, |svc, t| ..)`
  runs the closure **on the polling thread** right after each foreground sample (outside the driver
  lock), so per-sample driver reads (Monitor: `measure_extra_clocks`; Curve: voltage lock + ceiling)
  never block the UI thread; `tap.spawn(|svc| ..)` does a one-off read on its own thread;
  `tap.sender()` lets a `queue_custom` job post results back. `tap.drain()` each frame. Dropping it
  removes the listener.
* Hide to tray: title-bar −, or any minimise. Closes all child windows, remembering Monitor/Fan and
  reopening them on restore (deviation: WPF left owned windows alone). eframe runs no `ui()` while
  hidden, only `logic()`, which is where tray commands, show requests, telemetry and job results are
  drained.
* Poll detail (`update_poll_detail`): background mode unless Monitor or Fan is open; going to
  background clears `vm.telemetry` and the tray tooltip.

## Theme API

`theme::pal(ctx) -> Palette` (Copy) gives the current brushes: `bg panel panel_alt border
section_border text muted header highlight brand brand_hover selected title_hover close_hover
accent accent_dim warn danger`. `accent` = red GPU-information values and value boxes (v1.1.0).
Font: Consolas 12 (`theme::regular(BODY)`, bold = `theme::bold(BODY)` → consolab.ttf) from
`%WINDIR%\Fonts`, Segoe UI Symbol then egui defaults as fallbacks. The theme is switched by
`theme::apply(ctx, dark)`; `RochApp` persists it in `AppSettings::dark_mode`. Custom-drawn
controls (graphs, curve editors) should read `pal(ui.ctx())` every frame.

## View-model API (`MainViewModel`)

* Numeric editor values are addressed by `Field` (VoltageBoost, VoltageOffset, TargetVoltage,
  CoreOffset, MemoryOffset, PowerLimit, TempLimit, VoltageRailMax/Floor, MsvddRailMax/Floor,
  Xbar/Sys/VideoOffset, ClockLockMin/Max, FixedFan, Nvvdd/MsvddOcpAmps):
  `get(f)`, `set(f, v) -> bool` (snap via `clock_step::snap_within`, clamp, mark dirty — C#
  `SetTuned`), `bounds(f)`, `slider_range(f)`, `step(f)`, `input_text(f)`, `commit_input(f, &str)`,
  `nudge(f, dir)`, `nudge_spec("core:-1")`, `range_text(f)` (value-box tooltip).
* Capability switches: `has_voltage_boost, is_voltage_offset, has_curve_editor, has_xoc,
  has_voltage_rail, has_msvdd_rail, has_ocp, has_clock_lock, has_intel_clock_range, has_xbar,
  has_sys, has_video, has_temp_limit, has_zero_rpm, has_memory_timing`; `caps`, `device` are pub.
* Texts: identity lines (`driver_line`, `bios_line`, `pcie_bus_line`, `resizable_bar_line`,
  `memory_type_line`, `device_display_name`, `telemetry_device_name`), labels, range texts,
  `voltage_cap_text`, `target_voltage_range_text`, `xoc_status_text`, `limit_reason_text`,
  `applied_summary`, `status`/`status_is_error` (pub fields), `report(msg, is_error)`.
* Voltage: `stock_ceiling_mv`, `reachable_ceiling_mv`, `boost_ceiling_mv` (service-backed);
  `Field::TargetVoltage` is the cap the curve editor owns (0/"no cap" when ≥ reachable).
* Fans: `fan_mode_index/set_fan_mode_index` (0 Auto, 1 Fixed, 2 Curve),
  `is_fixed_fan/is_curve_fan`, `per_fan_percents`, `editor_curve/editor_curve_mut`,
  `mark_curve_dirty`, `zero_rpm/set_zero_rpm`, `sync_fans_from_service(mode, %, per_fan)`,
  `save_fans_to_active_slot(mode, %, per_fan, &curve) -> String`, `active_slot_name`.
* Telemetry: `set_telemetry(Option<GpuTelemetry>)` (also persists newly observed voltage ceilings
  into `AppSettings`), `telemetry()`.
* Profiles: `slots()`, `on_slot_clicked(n)`, `clear_slot(n)` (UI confirms first),
  `set_save_armed`, named profiles (`save_profile`, `load_selected_profile`,
  `delete_selected_profile`, `profiles`, `selected_profile`), startup (`apply_on_startup`,
  `set_apply_on_startup`, `startup_profile`, `set_startup_profile`).
* Actions that touch the driver **queue jobs**: `apply()`, `reset()`, `toggle_lever/set_lever`,
  slot click (load + apply), startup toggle. `RochApp` moves `take_jobs()` to a single worker thread
  (in order) and feeds each `Outcome` back to `finish()`. `busy()` / `busy_label()` disable write
  buttons and show "Applying…" in the footer. Tests: `run_jobs_inline()`.
* **Windows' own driver writes**: `vm.queue_custom(label, |shared| -> Box<dyn FnOnce(&mut
  MainViewModel) + Send>)` — the closure runs on the worker; the returned closure runs on the UI
  thread. Use it for `set_fans`, `set_vf_curve_targets`, `set_voltage_lock`, etc. To get results
  into a window's own state, capture an `Arc<Mutex<..>>` the window also holds, or use
  `vm.report()`.

## Part 2 windows

* **Monitor** — title "Roch GPU — Telemetry", 800×900, min 740×480. Owns a `Tap` (sample +
  `measure_extra_clocks`); on open it also renders `service.latest()` (a one-off read; older
  timestamps are dropped). The table is built from the first sample (`TelemetryTable::new(first,
  caps, extra_keys)`), then `add(t, extra)` per sample. Banding/hover use `highlight`/`panel`,
  which v1.1.0's palette makes equal to the background (so, as in v1.1.0, no visible bands). The
  scrollbar is the WPF grip: 9 px, muted at 30/65/80%. "Open at launch" writes
  `AppSettings::auto_open_monitor` at once. `RochApp::pump` no longer forwards samples to it.
* **Fan** — "Fan Control", width 560 (min 460), height to content. Radio mode is local until Apply
  (as WPF). Apply = one `queue_custom("Applying fans…")`: `set_fans(mode, %, per_fan, &curve,
  zero_rpm)`; on success `sync_fans_from_service` + `save_fans_to_active_slot`; the status comes back
  through an `Arc<Mutex<Option<(String, bool)>>>`. Per-fan captions from `on_telemetry`. Hardware
  curves (`fan_curve_is_hardware`) hide Hysteresis/Min step, and the editor refuses to add points
  past `fan_curve_points` (hint reads "N of M points"). The software curve is stepped by the
  service's poll (also in background mode), not by this window.
* **Curve** — "Voltage / Frequency curve", 960×620, min 700×460. Reads via `Tap` (curve on open /
  Reload / Intel Remove flatten; live V, clock, cap, ceiling per sample). Writes are `queue_custom`
  jobs whose UI half sets `Field::TargetVoltage` (flatten / remove, NVIDIA = `backend_name` starts
  with "NVIDIA", as C# `Backend is NvApiBackend`) or `Field::CoreOffset` (Intel `can_edit_vf_curve`
  after Apply/Reset) and posts `CurveMsg`s through `tap.sender()`. Keyboard: the plot takes focus on
  click and locks the arrow keys (`EventFilter`).
* Double-clicks in both editors are detected on the second *press* (0.3 s, 6 px), as WPF's
  `ClickCount == 2`, not egui's release-based `double_clicked()`.

## Done / not done

Done: startup flow (flags, detection, no-GPU prompt, recovery, logon apply, `--exit`), single
instance (`claim` + show-request listener), hide-to-tray + tray menu + tooltip, close-with-fan
prompt, light/dark theme with persisted preference and dark title bars on decorated windows,
main window (header, window buttons, all sliders incl. AMD offset/timing and Intel clock range,
profile bar with two-step Save, right-click clear, Startup + profile combo, Apply/Reset, pending
line, summary, footer links + status), XOC window, Graphics window, worker-thread writes, log file,
panic log.

Deviations: see the report in the PR/hand-off; notably `--mock` skips elevation; child windows close
on hide-to-tray (Monitor/Fan reopen on restore); dialogs are egui modals instead of MessageBoxes
(the no-GPU prompt and fatal errors use a native MessageBox); Enter in a value box commits and
keeps focus (same as WPF); no exe-embedded icon resource (window/tray icons are set at runtime —
adding one needs a `build.rs` with a resource compiler).

Part 2 done: Monitor (cards, table, switch, reset), Fan (Auto/Fixed/Curve, linked/per-fan
sliders with nudges, Zero RPM, curve editor with drag/add/remove, point rows with Enter/focus-out
commit and read-back, Default curve, hysteresis/min step, Apply + slot save), Curve (drag, Ctrl
flatten, Shift tail, double-click/right-click reset, arrow keys with Shift/Ctrl, cap/ceiling/
unreachable/extrapolation/live markers, flatten via lock on NVIDIA or per point elsewhere, Remove
flatten, Reload, Reset, Apply with read-back, Intel step/range/core-offset sync). Unit tests:
`telemetry_table.rs` (all of `GpuTuner.Telemetry.Tests` + NVIDIA rails/domains), `fan_editor.rs`,
`vf_editor.rs`, `windows/fan.rs`, `windows/monitor.rs`. The App-test fan-sync checks were already in
`tests.rs` (part 1).

Part 2 deviations: Monitor's extra-clock reads and Curve's per-sample cap/ceiling reads run on the
poll thread, and the curve read/writes off the UI thread (WPF: all on the UI thread); the hardware
fan-curve point limit is enforced in the editor (WPF left it to the backend's error); `F<n>` number
formatting uses Rust's round-half-even on exact ties (.NET rounds those away from zero) — only
visible on exactly representable ties such as 0.125 at two decimals.

Not done: exe resource icon. Not exercised against real hardware: the NVIDIA voltage-lock flatten
path and the cap drawing (mock is not NVIDIA; the logic is unit-tested).
