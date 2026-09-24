using GpuTuner.App.ViewModels;
using GpuTuner.Core.Backends.Mock;
using GpuTuner.Core.Models;
using GpuTuner.Core.Services;
using System.IO;

var folder = Path.Combine(Path.GetTempPath(), "RochGpuFanSyncTests-" + Guid.NewGuid().ToString("N"));
int passed = 0;
void Check(string name, bool ok)
{
    if (!ok) throw new InvalidOperationException(name);
    passed++;
}
try
{
    using var service = new TuningService(new MockBackend());
    service.Initialize();
    var vm = new MainViewModel(service, new ProfileStore(folder));
    // Start with a different old value, exactly as when the fan window owns its own slider.
    vm.FixedFan = 35;
    var errors = service.SetFans(FanMode.Fixed, 65, [], vm.EditorCurve);
    Check("fan-only apply succeeds", errors.Count == 0);
    vm.SyncFansFromService(FanMode.Fixed, 65, []);
    Check("linked fan speed synchronized", vm.FixedFan == 65);
    vm.PowerLimit = 95;
    var next = vm.BuildProfileFromEditor("Power change");
    Check("power profile retains selected fixed fan speed", next.FanMode == FanMode.Fixed && next.FixedFanPercent == 65 && next.FixedFanPercents.Length == 0);
    Check("main apply succeeds", service.Apply(next).Count == 0);
    var actual = service.Backend.ReadTuningState(0);
    Check("power changes while fan stays fixed", actual.PowerLimitPercent == 95 && actual.FanManual && actual.FanPercent == 65);
    int[] separate = [60, 75];
    vm.SyncFansFromService(FanMode.Fixed, 60, separate);
    separate[0] = 10;
    next = vm.BuildProfileFromEditor("Separate fans");
    Check("individual fan settings are copied", next.FixedFanPercent == 60 && next.FixedFanPercents.SequenceEqual(new[] {60,75}));
    vm.SyncFansFromService(FanMode.Fixed, 70, []);
    next = vm.BuildProfileFromEditor("Relink fans");
    Check("relinked speed replaces old individual settings", next.FixedFanPercent == 70 && next.FixedFanPercents.Length == 0);
    vm.SyncFansFromService(FanMode.Auto, 70, []);
    Check("automatic mode still synchronizes", vm.BuildProfileFromEditor("Auto").FanMode == FanMode.Auto);
    // Use Intel capabilities without touching a physical GPU.
    typeof(MainViewModel).GetField("<Caps>k__BackingField", System.Reflection.BindingFlags.Instance | System.Reflection.BindingFlags.NonPublic)!.SetValue(vm,
        vm.Caps with { VoltageStyle = VoltageControlStyle.Percent, CoreOffsetStepMhz = 1, CoreOffsetMinMhz = -300,
            MemoryClockIsAbsolute = true, MemoryOffsetMinMhz = 19000, MemoryOffsetMaxMhz = 22000,
            TempLimitMinC = 60, TempLimitMaxC = 100, PowerLimitMaxPercent = 120 });
    vm.LoadIntoEditor(new TuningProfile { CoreOffsetMhz = 91, MemoryOffsetMhz = 19001, VoltageBoostPercent = 41, PowerLimitPercent = 101, TempLimitC = 99 });
    Check("loading Intel settings preserves off-grid values", vm.CoreOffset == 91 && vm.MemoryOffset == 19001 && vm.VoltageBoost == 41 && vm.PowerLimit == 101 && vm.TempLimit == 99);
    vm.CoreOffset = 90; vm.MemoryOffset = 19000; vm.VoltageBoost = 40; vm.PowerLimit = 100; vm.TempLimit = 90;
    foreach (var key in new[] { "core", "mem", "voltboost", "power", "temp" }) vm.NudgeCommand.Execute(key + ":1");
    Check("Intel core step is 15 MHz", vm.CoreOffset == 105);
    Check("Intel memory step is 100 Mbps", vm.MemoryOffset == 19100);
    Check("Intel voltage step is 5 percent", vm.VoltageBoost == 45);
    Check("Intel power step is 5 percent", vm.PowerLimit == 105);
    Check("Intel temperature step is 5 percent", vm.TempLimit == 95);
    vm.MemoryOffset = 22099; vm.CoreOffset = -999; vm.PowerLimit = 999; vm.TempLimit = 999;
    Check("Intel edits stay inside driver ranges", vm.MemoryOffset == 22000 && vm.CoreOffset == -300 && vm.PowerLimit == 120 && vm.TempLimit == 100);
    vm.IntelClockRangeEnabled = true;
    vm.ClockLockMin = vm.Caps.ClockLockMinMhz; vm.ClockLockMax = vm.Caps.ClockLockMaxMhz;
    next = vm.BuildProfileFromEditor("Intel range");
    Check("Intel full hardware range is explicit, not factory reset", next.ClockLockMinMhz > 0 && next.ClockLockMaxMhz > 0 && next.XocArmed.Has(XocLever.ClockRange));
    vm.IntelClockRangeEnabled = false;
    next = vm.BuildProfileFromEditor("Factory range");
    Check("Intel disabled range requests factory defaults", next.ClockLockMinMhz == 0 && next.ClockLockMaxMhz == 0 && !next.XocArmed.Has(XocLever.ClockRange));
    vm.LoadIntoEditor(new TuningProfile { XocArmed = XocLever.ClockRange, ClockLockMinMhz = 500, ClockLockMaxMhz = 2300 });
    Check("Intel clock-range profile round trip", vm.IntelClockRangeEnabled && vm.BuildProfileFromEditor("Restored range").ClockLockMinMhz == 500 && vm.ClockLockMax == 2300);
    GraphicsLayoutChecks.Run(Check);
    Console.WriteLine($"App: {passed} passed, 0 failed");
}
finally
{
    if (Directory.Exists(folder)) Directory.Delete(folder, recursive: true);
}
