# Roch GPU - AMD probe 2
#
# Probe 1 said Overdrive VERSION=8 but the OD8 getters returned -9 (ADL_ERR_NULL_POINTER) while
# PMLog succeeded on the same context and adapter. So this round tests the two things that would
# explain that: (a) OD8 wants a different adapter index, (b) this driver exports a different set of
# OD8 entry points than the ones tried. It also reads live sensors.
#
# Read-only. Writes whats-amd2.txt to your Desktop.

$ErrorActionPreference = 'Continue'
$out = Join-Path ([Environment]::GetFolderPath('Desktop')) 'whats-amd2.txt'
$lines = New-Object System.Collections.Generic.List[string]
function W($s) { $lines.Add([string]$s) }

W "===== Roch GPU - AMD probe 2 ====="
W (Get-Date).ToString('yyyy-MM-dd HH:mm:ss')
W ""

# ---------------------------------------------------------------- exported entry points
W "===== 1. tuning entry points present in atiadlxx.dll ====="
try {
    $dll = Join-Path $env:SystemRoot 'System32\atiadlxx.dll'
    $bytes = [System.IO.File]::ReadAllBytes($dll)
    $text = [System.Text.Encoding]::GetEncoding(28591).GetString($bytes)
    $names = [regex]::Matches($text, 'ADL2?_[A-Za-z0-9_]{4,60}') | ForEach-Object { $_.Value }
    $interesting = $names | Where-Object { $_ -match 'Overdrive|PerfTuning|PMLog|PowerTune|Tuning' } |
                   Sort-Object -Unique
    W ("  {0} matching names" -f $interesting.Count)
    foreach ($n in $interesting) { W ("    {0}" -f $n) }
} catch { W "  <$($_.Exception.Message)>" }
W ""

$cs = @'
using System;
using System.Runtime.InteropServices;

public static class Adl2
{
    public delegate IntPtr MemAlloc(int size);

    [DllImport("atiadlxx.dll", CallingConvention = CallingConvention.Cdecl)]
    public static extern int ADL2_Main_Control_Create(MemAlloc cb, int enumConnected, out IntPtr context);
    [DllImport("atiadlxx.dll", CallingConvention = CallingConvention.Cdecl)]
    public static extern int ADL2_Main_Control_Destroy(IntPtr context);
    [DllImport("atiadlxx.dll", CallingConvention = CallingConvention.Cdecl)]
    public static extern int ADL2_Adapter_NumberOfAdapters_Get(IntPtr context, out int num);
    [DllImport("atiadlxx.dll", CallingConvention = CallingConvention.Cdecl)]
    public static extern int ADL2_Adapter_AdapterInfo_Get(IntPtr context, IntPtr info, int inputSize);
    [DllImport("atiadlxx.dll", CallingConvention = CallingConvention.Cdecl)]
    public static extern int ADL2_Overdrive_Caps(IntPtr context, int adapterIndex, ref int supported, ref int enabled, ref int version);

    // caller-allocated variants: we hand over a generously sized buffer, so a wrong idea of the
    // feature count on this generation cannot overflow anything.
    [DllImport("atiadlxx.dll", CallingConvention = CallingConvention.Cdecl)]
    public static extern int ADL2_Overdrive8_Init_Setting_Get(IntPtr context, int adapterIndex, IntPtr initSetting);
    [DllImport("atiadlxx.dll", CallingConvention = CallingConvention.Cdecl)]
    public static extern int ADL2_Overdrive8_Current_Setting_Get(IntPtr context, int adapterIndex, IntPtr currentSetting);

    // driver-allocated variants
    [DllImport("atiadlxx.dll", CallingConvention = CallingConvention.Cdecl)]
    public static extern int ADL2_Overdrive8_Init_SettingX2_Get(IntPtr context, int adapterIndex,
        ref int caps, ref int numFeatures, ref IntPtr list);
    [DllImport("atiadlxx.dll", CallingConvention = CallingConvention.Cdecl)]
    public static extern int ADL2_Overdrive8_Current_SettingX2_Get(IntPtr context, int adapterIndex,
        ref int numFeatures, ref IntPtr list);
    [DllImport("atiadlxx.dll", CallingConvention = CallingConvention.Cdecl)]
    public static extern int ADL2_Overdrive8_Current_SettingX3_Get(IntPtr context, int adapterIndex,
        ref int notAdjustableBits, ref int numSettings, ref IntPtr list, int option);

    [DllImport("atiadlxx.dll", CallingConvention = CallingConvention.Cdecl)]
    public static extern int ADL2_New_QueryPMLogData_Get(IntPtr context, int adapterIndex, IntPtr dataOutput);

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Ansi)]
    public struct AdapterInfo
    {
        public int iSize;
        public int iAdapterIndex;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 256)] public string strUDID;
        public int iBusNumber;
        public int iDeviceNumber;
        public int iFunctionNumber;
        public int iVendorID;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 256)] public string strAdapterName;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 256)] public string strDisplayName;
        public int iPresent;
        public int iExist;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 256)] public string strDriverPath;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 256)] public string strDriverPathExt;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 256)] public string strPNPString;
        public int iOSDisplayIndex;
    }

    private static IntPtr Alloc(int size) { return Marshal.AllocCoTaskMem(size); }
    public static MemAlloc Allocator = new MemAlloc(Alloc);
}
'@
Add-Type -TypeDefinition $cs -ErrorAction Stop

$od8Names = @(
 'GFXCLK_FMAX','GFXCLK_FMIN','GFXCLK_FREQ1','GFXCLK_VOLTAGE1','GFXCLK_FREQ2','GFXCLK_VOLTAGE2',
 'GFXCLK_FREQ3','GFXCLK_VOLTAGE3','UCLK_FMAX','POWER_PERCENTAGE','FAN_MIN_SPEED','FAN_ACOUSTIC_LIMIT',
 'FAN_TARGET_TEMP','OPERATING_TEMP_MAX','AC_TIMING','FAN_ZERORPM_CONTROL','AUTO_UV_ENGINE_CONTROL',
 'AUTO_OC_ENGINE_CONTROL','AUTO_OC_MEMORY_CONTROL','FAN_CURVE_TEMPERATURE_1','FAN_CURVE_SPEED_1',
 'FAN_CURVE_TEMPERATURE_2','FAN_CURVE_SPEED_2','FAN_CURVE_TEMPERATURE_3','FAN_CURVE_SPEED_3',
 'FAN_CURVE_TEMPERATURE_4','FAN_CURVE_SPEED_4','FAN_CURVE_TEMPERATURE_5','FAN_CURVE_SPEED_5',
 'UCLK_FMIN','POWER_TDC_LIMIT','FULL_CONTROL_MODE')

function Rc($c) {
    switch ($c) {
        0 { 'OK' } -1 { 'ERR' } -2 { 'NOT_INIT' } -3 { 'INVALID_PARAM' } -4 { 'INVALID_PARAM_SIZE' }
        -5 { 'INVALID_ADL_IDX' } -8 { 'NOT_SUPPORTED' } -9 { 'NULL_POINTER' } -10 { 'DISABLED_ADAPTER' }
        default { "rc$c" }
    }
}

W "===== 2. OD8 across every adapter index ====="
$ctx = [IntPtr]::Zero
$rc = [Adl2]::ADL2_Main_Control_Create([Adl2]::Allocator, 1, [ref]$ctx)
W ("  ADL2_Main_Control_Create -> {0}" -f (Rc $rc))

if ($rc -eq 0 -and $ctx -ne [IntPtr]::Zero) {
    $num = 0
    $null = [Adl2]::ADL2_Adapter_NumberOfAdapters_Get($ctx, [ref]$num)
    $sz = [System.Runtime.InteropServices.Marshal]::SizeOf([type][Adl2+AdapterInfo])
    $buf = [System.Runtime.InteropServices.Marshal]::AllocHGlobal($sz * $num)
    $big = [System.Runtime.InteropServices.Marshal]::AllocHGlobal(65536)

    try {
        $null = [Adl2]::ADL2_Adapter_AdapterInfo_Get($ctx, $buf, $sz * $num)

        for ($i = 0; $i -lt $num; $i++) {
            $ai = [System.Runtime.InteropServices.Marshal]::PtrToStructure([IntPtr]::Add($buf, $i * $sz), [type][Adl2+AdapterInfo])
            if ($ai.iVendorID -ne 1002) { continue }
            $idx = $ai.iAdapterIndex

            $sup = 0; $en = 0; $ver = 0
            $r0 = [Adl2]::ADL2_Overdrive_Caps($ctx, $idx, [ref]$sup, [ref]$en, [ref]$ver)

            W ""
            W ("  --- adapter index {0}  (bus {1} dev {2} fn {3})  {4} ---" -f $idx, $ai.iBusNumber, $ai.iDeviceNumber, $ai.iFunctionNumber, $ai.strAdapterName)
            W ("      Overdrive_Caps      -> {0}  supported={1} enabled={2} version={3}" -f (Rc $r0), $sup, $en, $ver)

            # (a) driver-allocated
            $caps = 0; $nf = 0; $lp = [IntPtr]::Zero
            $r = [Adl2]::ADL2_Overdrive8_Init_SettingX2_Get($ctx, $idx, [ref]$caps, [ref]$nf, [ref]$lp)
            W ("      Init_SettingX2      -> {0}  caps=0x{1:X8} features={2}" -f (Rc $r), $caps, $nf)
            if ($r -eq 0 -and $nf -gt 0 -and $lp -ne [IntPtr]::Zero) {
                for ($k = 0; $k -lt $nf; $k++) {
                    $e = [IntPtr]::Add($lp, $k * 16)
                    $fid = [System.Runtime.InteropServices.Marshal]::ReadInt32($e, 0)
                    $mn = [System.Runtime.InteropServices.Marshal]::ReadInt32($e, 4)
                    $mx = [System.Runtime.InteropServices.Marshal]::ReadInt32($e, 8)
                    $df = [System.Runtime.InteropServices.Marshal]::ReadInt32($e, 12)
                    $nm = if ($fid -ge 0 -and $fid -lt $od8Names.Count) { $od8Names[$fid] } else { "id$fid" }
                    W ("        [{0,2}] {1,-24} min={2,-8} max={3,-8} default={4}" -f $fid, $nm, $mn, $mx, $df)
                }
            }

            # (b) caller-allocated, generous buffer
            [System.Runtime.InteropServices.Marshal]::Copy((New-Object byte[] 65536), 0, $big, 65536)
            $r = [Adl2]::ADL2_Overdrive8_Init_Setting_Get($ctx, $idx, $big)
            W ("      Init_Setting (v1)   -> {0}" -f (Rc $r))
            if ($r -eq 0) {
                $cnt = [System.Runtime.InteropServices.Marshal]::ReadInt32($big, 0)
                $cb  = [System.Runtime.InteropServices.Marshal]::ReadInt32($big, 4)
                W ("        count={0}  capabilities=0x{1:X8}" -f $cnt, $cb)
                if ($cnt -gt 0 -and $cnt -le 64) {
                    for ($k = 0; $k -lt $cnt; $k++) {
                        $o = 8 + $k * 16
                        $fid = [System.Runtime.InteropServices.Marshal]::ReadInt32($big, $o)
                        $mn = [System.Runtime.InteropServices.Marshal]::ReadInt32($big, $o + 4)
                        $mx = [System.Runtime.InteropServices.Marshal]::ReadInt32($big, $o + 8)
                        $df = [System.Runtime.InteropServices.Marshal]::ReadInt32($big, $o + 12)
                        $nm = if ($k -lt $od8Names.Count) { $od8Names[$k] } else { "id$k" }
                        W ("        [{0,2}] {1,-24} featureId={2,-4} min={3,-8} max={4,-8} default={5}" -f $k, $nm, $fid, $mn, $mx, $df)
                    }
                }
            }

            $nc = 0; $cp = [IntPtr]::Zero
            $r = [Adl2]::ADL2_Overdrive8_Current_SettingX2_Get($ctx, $idx, [ref]$nc, [ref]$cp)
            W ("      Current_SettingX2   -> {0}  count={1}" -f (Rc $r), $nc)
            if ($r -eq 0 -and $nc -gt 0 -and $cp -ne [IntPtr]::Zero) {
                for ($k = 0; $k -lt $nc; $k++) {
                    $val = [System.Runtime.InteropServices.Marshal]::ReadInt32($cp, $k * 4)
                    $nm = if ($k -lt $od8Names.Count) { $od8Names[$k] } else { "id$k" }
                    W ("        [{0,2}] {1,-24} = {2}" -f $k, $nm, $val)
                }
            }

            [System.Runtime.InteropServices.Marshal]::Copy((New-Object byte[] 65536), 0, $big, 65536)
            $r = [Adl2]::ADL2_Overdrive8_Current_Setting_Get($ctx, $idx, $big)
            W ("      Current_Setting(v1) -> {0}" -f (Rc $r))
            if ($r -eq 0) {
                $cnt = [System.Runtime.InteropServices.Marshal]::ReadInt32($big, 0)
                W ("        count={0}" -f $cnt)
                if ($cnt -gt 0 -and $cnt -le 64) {
                    for ($k = 0; $k -lt $cnt; $k++) {
                        $val = [System.Runtime.InteropServices.Marshal]::ReadInt32($big, 4 + $k * 4)
                        $nm = if ($k -lt $od8Names.Count) { $od8Names[$k] } else { "id$k" }
                        W ("        [{0,2}] {1,-24} = {2}" -f $k, $nm, $val)
                    }
                }
            }

            $na = 0; $ns = 0; $x3 = [IntPtr]::Zero
            $r = [Adl2]::ADL2_Overdrive8_Current_SettingX3_Get($ctx, $idx, [ref]$na, [ref]$ns, [ref]$x3, 0)
            W ("      Current_SettingX3   -> {0}  notAdjustable=0x{1:X8} count={2}" -f (Rc $r), $na, $ns)
        }

        # ---------------------------------------------------------------- live sensors
        W ""
        W "===== 3. PMLog live sensors (adapter 0) ====="
        [System.Runtime.InteropServices.Marshal]::Copy((New-Object byte[] 65536), 0, $big, 65536)
        $r = [Adl2]::ADL2_New_QueryPMLogData_Get($ctx, 0, $big)
        W ("  ADL2_New_QueryPMLogData_Get -> {0}" -f (Rc $r))
        if ($r -eq 0) {
            $size = [System.Runtime.InteropServices.Marshal]::ReadInt32($big, 0)
            W ("  size field = {0}" -f $size)
            for ($k = 0; $k -lt 256; $k++) {
                $o = 4 + $k * 8
                $supported = [System.Runtime.InteropServices.Marshal]::ReadInt32($big, $o)
                $value = [System.Runtime.InteropServices.Marshal]::ReadInt32($big, $o + 4)
                if ($supported -ne 0) { W ("    sensor[{0,3}] = {1}" -f $k, $value) }
            }
        }
    } finally {
        [System.Runtime.InteropServices.Marshal]::FreeHGlobal($buf)
        [System.Runtime.InteropServices.Marshal]::FreeHGlobal($big)
    }
    $null = [Adl2]::ADL2_Main_Control_Destroy($ctx)
}

W ""
W "===== end ====="
Set-Content -Path $out -Value $lines -Encoding UTF8
Write-Host ""
Write-Host "Wrote $out"
Write-Host "Upload that file to Claude."
Write-Host ""
