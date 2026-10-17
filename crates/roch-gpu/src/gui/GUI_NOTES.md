# GUI port notes (WPF `GpuTuner.App` → eframe/egui)

Handoff file for the GUI port. Part 1 (this) = app shell, theme, tray, view model, main window,
XOC and Graphics windows. Part 2 = Monitor, Fan and V/F Curve windows (currently stubs).

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
  logon task), plus unit tests in `format.rs`, `assets.rs`, `startup.rs`, `windows/graphics.rs`.

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
| `windows/monitor.rs` | `MonitorWindow`, `TelemetryTable` | **stub** |
| `windows/fan.rs` | `FanWindow`, `FanCurveEditor` | **stub** |
| `windows/curve.rs` | `CurveWindow`, `VfCurveEditor` | **stub** |
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
  (XOC, Fan, Curve).
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
* Fans (for part 2): `fan_mode_index/set_fan_mode_index` (0 Auto, 1 Fixed, 2 Curve),
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

## What part 2 must fill in

1. **`windows/monitor.rs`** — port `MonitorWindow.xaml(.cs)` + `ViewModels/TelemetryTable.cs`.
   Samples arrive via `MonitorWindow::on_telemetry(&GpuTelemetry)` (called by `RochApp::pump` for
   every foreground sample, before drawing); history is `shared.service.history()`. Keep `id()`,
   `focus()`, `show()` signatures. Title "Roch GPU — Telemetry", 800×900, min 740×480.
2. **`windows/fan.rs`** — port `FanWindow` + `Controls/FanCurveEditor.cs` (a custom-painted curve
   editor). Fan state is in the view model (see above); write through `vm.queue_custom` with
   `shared.service.set_fans(mode, %, per_fan, &curve, zero_rpm)`, then `sync_fans_from_service` and
   `save_fans_to_active_slot`. Samples via `FanWindow::on_telemetry`. Title "Fan Control", width 560
   (min 460), height to content (`ChildChrome::fit`).
3. **`windows/curve.rs`** — port `CurveWindow` + `Controls/VfCurveEditor.cs`. It edits
   `Field::TargetVoltage` / `Field::CoreOffset` and calls `read_vf_curve`, `set_vf_curve_targets`,
   `set_voltage_lock`, `read_tuning_state` (put writes on `queue_custom`). Title "Voltage / Frequency
   curve", 960×620, min 700×460.
4. Port the remaining WPF test checks for those windows (TelemetryTable etc.) into unit tests.

## Done / not done (part 1)

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

Not done: Monitor/Fan/Curve windows (part 2); exe resource icon.
