# Roch GPU - AMD probe 3
#
# Probe 2's OD8 getters failed with -9 because lpNumberOfFeatures is an IN/OUT parameter: it must be
# pre-set to OD8_COUNT-2 (75) before the call. This round does that, and dumps the full feature table
# with the real min/max/default the driver reports, so the sliders get built from measured ranges.
#
# Read-only. Writes whats-amd3.txt to your Desktop.

$ErrorActionPreference = 'Continue'
$out = Join-Path ([Environment]::GetFolderPath('Desktop')) 'whats-amd3.txt'
$lines = New-Object System.Collections.Generic.List[string]
function W($s) { $lines.Add([string]$s) }

$OD8_COUNT = 77
$NFEAT = $OD8_COUNT - 2

# ADLOD8SettingId, verbatim from adl_defines.h
$id = @(
 'GFXCLK_FMAX','GFXCLK_FMIN','GFXCLK_FREQ1','GFXCLK_VOLTAGE1','GFXCLK_FREQ2','GFXCLK_VOLTAGE2',
 'GFXCLK_FREQ3','GFXCLK_VOLTAGE3','UCLK_FMAX','POWER_PERCENTAGE','FAN_MIN_SPEED','FAN_ACOUSTIC_LIMIT',
 'FAN_TARGET_TEMP','OPERATING_TEMP_MAX','AC_TIMING','FAN_ZERORPM_CONTROL','AUTO_UV_ENGINE_CONTROL',
 'AUTO_OC_ENGINE_CONTROL','AUTO_OC_MEMORY_CONTROL','FAN_CURVE_TEMPERATURE_1','FAN_CURVE_SPEED_1',
 'FAN_CURVE_TEMPERATURE_2','FAN_CURVE_SPEED_2','FAN_CURVE_TEMPERATURE_3','FAN_CURVE_SPEED_3',
 'FAN_CURVE_TEMPERATURE_4','FAN_CURVE_SPEED_4','FAN_CURVE_TEMPERATURE_5','FAN_CURVE_SPEED_5',
 'WS_FAN_AUTO_FAN_ACOUSTIC_LIMIT','GFXCLK_CURVE_COEFFICIENT_A','GFXCLK_CURVE_COEFFICIENT_B',
 'GFXCLK_CURVE_COEFFICIENT_C','GFXCLK_CURVE_VFT_FMIN','UCLK_FMIN','FAN_ZERO_RPM_STOP_TEMPERATURE',
 'OPTIMZED_POWER_MODE','OD_VOLTAGE','ADV_OC_LIMITS_SETTING','PER_ZONE_GFX_VOLTAGE_OFFSET_POINT_1',
 'PER_ZONE_GFX_VOLTAGE_OFFSET_POINT_2','PER_ZONE_GFX_VOLTAGE_OFFSET_POINT_3','PER_ZONE_GFX_VOLTAGE_OFFSET_POINT_4',
 'PER_ZONE_GFX_VOLTAGE_OFFSET_POINT_5','PER_ZONE_GFX_VOLTAGE_OFFSET_POINT_6','AUTO_CURVE_OPTIMIZER_SETTING',
 'GFX_VOLTAGE_LIMIT_SETTING','TDC_PERCENTAGE','FULL_CONTROL_MODE_SETTING','FULL_CONTROL_MODE_GFXCLK',
 'FULL_CONTROL_MODE_UCLK','IDLE_POWER_SAVING_FEATURE_CONTROL','RUNTIME_POWER_SAVING_FEATURE_CONTROL',
 'FULL_CONTROL_MODE_FEATURE_CONTROL','PZ_VOLT_OFFSET_FREQ_ANCHOR_1','PZ_VOLT_OFFSET_FREQ_ANCHOR_2',
 'PZ_VOLT_OFFSET_FREQ_ANCHOR_3','PZ_VOLT_OFFSET_FREQ_ANCHOR_4','PZ_VOLT_OFFSET_FREQ_ANCHOR_5',
 'PZ_VOLT_OFFSET_FREQ_ANCHOR_6','PZ_VOLT_OFFSET_VOLTAGE_LIMIT','ACTIMING_TRRDS','ACTIMING_TCL',
 'ACTIMING_TCWL','ACTIMING_TRCDRD','ACTIMING_TRCDWR','ACTIMING_TRAS','ACTIMING_TRPAB','ACTIMING_TRFC',
 'ACTIMING_TRFCPB','ACTIMING_TRREFD','ACTIMING_TREF','ACTIMING_TWR','ACTIMING_TWTRS',
 'OVERDRIVE_INTERFACE_ID','AUTO_UV_ENGINE_V2_ID','POWER_GAUGE')

# ADLOD8FeatureControl capability bits
$capBits = @{
  0='GFXCLK_LIMITS'; 1='GFXCLK_CURVE'; 2='UCLK_MAX'; 3='POWER_LIMIT'; 4='ACOUSTIC_LIMIT_SCLK';
  5='FAN_SPEED_MIN'; 6='TEMPERATURE_FAN'; 7='TEMPERATURE_SYSTEM'; 8='MEMORY_TIMING_TUNE';
  9='FAN_ZERO_RPM_CONTROL'; 10='AUTO_UV_ENGINE'; 11='AUTO_OC_ENGINE'; 12='AUTO_OC_MEMORY';
  13='FAN_CURVE'; 14='WS_AUTO_FAN_ACOUSTIC_LIMIT'; 15='GFXCLK_QUADRATIC_CURVE';
  16='OPTIMIZED_GPU_POWER_MODE'; 17='ODVOLTAGE_LIMIT'; 18='ADV_OC_LIMITS';
  19='PER_ZONE_GFX_VOLTAGE_OFFSET'; 20='AUTO_CURVE_OPTIMIZER'; 21='GFX_VOLTAGE_LIMIT';
  22='TDC_LIMIT'; 23='FULL_CONTROL_MODE'; 24='POWER_SAVING_FEATURE_CONTROL';
  25='ACTIMING_PARAMETERS_TUNE'; 26='OVERDRIVE_INTERFACE'; 27='AUTO_UV_ENGINE_V2'; 28='POWER_GAUGE'
}

W "===== Roch GPU - AMD probe 3 ====="
W (Get-Date).ToString('yyyy-MM-dd HH:mm:ss')
W ""

$cs = @'
using System;
using System.Runtime.InteropServices;

public static class Adl3
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
    public static extern int ADL2_Overdrive8_Init_SettingX2_Get(IntPtr context, int adapterIndex,
        ref int caps, ref int numFeatures, ref IntPtr list);
    [DllImport("atiadlxx.dll", CallingConvention = CallingConvention.Cdecl)]
    public static extern int ADL2_Overdrive8_Current_SettingX2_Get(IntPtr context, int adapterIndex,
        ref int numFeatures, ref IntPtr list);
    [DllImport("atiadlxx.dll", CallingConvention = CallingConvention.Cdecl)]
    public static extern int ADL2_Overdrive8_Current_SettingX3_Get(IntPtr context, int adapterIndex,
        ref int notAdjustableBits, ref int numSettings, ref IntPtr list, int option);
    [DllImport("atiadlxx.dll", CallingConvention = CallingConvention.Cdecl)]
    public static extern int ADL2_Overdrive8_Init_Setting_Get(IntPtr context, int adapterIndex, IntPtr initSetting);

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Ansi)]
    public struct AdapterInfo
    {
        public int iSize; public int iAdapterIndex;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 256)] public string strUDID;
        public int iBusNumber; public int iDeviceNumber; public int iFunctionNumber; public int iVendorID;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 256)] public string strAdapterName;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 256)] public string strDisplayName;
        public int iPresent; public int iExist;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 256)] public string strDriverPath;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 256)] public string strDriverPathExt;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 256)] public string strPNPString;
        public int iOSDisplayIndex;
    }

    private static IntPtr A(int size) { return Marshal.AllocCoTaskMem(size); }
    public static MemAlloc Allocator = new MemAlloc(A);
}
'@
Add-Type -TypeDefinition $cs -ErrorAction Stop

function Rc($c) {
    switch ($c) { 0 {'OK'} -1 {'ERR'} -2 {'NOT_INIT'} -3 {'INVALID_PARAM'} -4 {'INVALID_PARAM_SIZE'}
                  -5 {'INVALID_ADL_IDX'} -8 {'NOT_SUPPORTED'} -9 {'NULL_POINTER'} default {"rc$c"} }
}

$ctx = [IntPtr]::Zero
$rc = [Adl3]::ADL2_Main_Control_Create([Adl3]::Allocator, 1, [ref]$ctx)
W ("ADL2_Main_Control_Create -> {0}" -f (Rc $rc))

if ($rc -eq 0) {
    $num = 0
    $null = [Adl3]::ADL2_Adapter_NumberOfAdapters_Get($ctx, [ref]$num)
    $sz = [System.Runtime.InteropServices.Marshal]::SizeOf([type][Adl3+AdapterInfo])
    $buf = [System.Runtime.InteropServices.Marshal]::AllocHGlobal($sz * $num)
    $big = [System.Runtime.InteropServices.Marshal]::AllocHGlobal(65536)
    try {
        $null = [Adl3]::ADL2_Adapter_AdapterInfo_Get($ctx, $buf, $sz * $num)
        $idx = -1
        for ($i = 0; $i -lt $num; $i++) {
            $ai = [System.Runtime.InteropServices.Marshal]::PtrToStructure([IntPtr]::Add($buf, $i * $sz), [type][Adl3+AdapterInfo])
            if ($ai.iVendorID -eq 1002) { $idx = $ai.iAdapterIndex; $nm = $ai.strAdapterName; break }
        }
        W ("adapter index {0}: {1}" -f $idx, $nm)
        W ""

        # ---- capabilities + per-feature ranges
        $caps = 0; $nf = $NFEAT; $lp = [IntPtr]::Zero
        $r = [Adl3]::ADL2_Overdrive8_Init_SettingX2_Get($ctx, $idx, [ref]$caps, [ref]$nf, [ref]$lp)
        W ("===== Init_SettingX2 (numFeatures pre-set to {0}) -> {1} =====" -f $NFEAT, (Rc $r))
        W ("  capabilities = 0x{0:X8}" -f $caps)
        foreach ($b in ($capBits.Keys | Sort-Object)) {
            if ($caps -band (1 -shl $b)) { W ("    bit {0,2}  {1}" -f $b, $capBits[$b]) }
        }
        W ("  features returned = {0}" -f $nf)

        # current values, same in/out treatment
        $nc = $NFEAT; $cp = [IntPtr]::Zero
        $r2 = [Adl3]::ADL2_Overdrive8_Current_SettingX2_Get($ctx, $idx, [ref]$nc, [ref]$cp)
        W ""
        W ("===== Current_SettingX2 -> {0}  count={1} =====" -f (Rc $r2), $nc)

        W ""
        W "===== feature table:  id  name  [min .. max]  default  current ====="
        $n = if ($nf -gt 0) { $nf } else { $NFEAT }
        for ($k = 0; $k -lt $n; $k++) {
            $nmk = if ($k -lt $id.Count) { $id[$k] } else { "id$k" }
            $fid = '?'; $mn = '?'; $mx = '?'; $df = '?'
            if ($r -eq 0 -and $lp -ne [IntPtr]::Zero) {
                $e = [IntPtr]::Add($lp, $k * 16)
                $fid = [System.Runtime.InteropServices.Marshal]::ReadInt32($e, 0)
                $mn  = [System.Runtime.InteropServices.Marshal]::ReadInt32($e, 4)
                $mx  = [System.Runtime.InteropServices.Marshal]::ReadInt32($e, 8)
                $df  = [System.Runtime.InteropServices.Marshal]::ReadInt32($e, 12)
            }
            $cur = '?'
            if ($r2 -eq 0 -and $cp -ne [IntPtr]::Zero -and $k -lt $nc) {
                $cur = [System.Runtime.InteropServices.Marshal]::ReadInt32($cp, $k * 4)
            }
            # a feature the driver does not expose reports min == max
            $mark = if ($mn -ne '?' -and $mn -eq $mx) { '   (locked)' } else { '' }
            W ("  [{0,2}] {1,-38} fid={2,-4} [{3,8} .. {4,8}]  def={5,-8} cur={6}{7}" -f $k, $nmk, $fid, $mn, $mx, $df, $cur, $mark)
        }

        # ---- X3 tells us which are currently not adjustable
        $na = 0; $ns = $NFEAT; $x3 = [IntPtr]::Zero
        $r3 = [Adl3]::ADL2_Overdrive8_Current_SettingX3_Get($ctx, $idx, [ref]$na, [ref]$ns, [ref]$x3, 0)
        W ""
        W ("===== Current_SettingX3 -> {0}  notAdjustableBits=0x{1:X8}  count={2} =====" -f (Rc $r3), $na, $ns)
    } finally {
        [System.Runtime.InteropServices.Marshal]::FreeHGlobal($buf)
        [System.Runtime.InteropServices.Marshal]::FreeHGlobal($big)
    }
    $null = [Adl3]::ADL2_Main_Control_Destroy($ctx)
}

W ""
W "===== end ====="
Set-Content -Path $out -Value $lines -Encoding UTF8
Write-Host ""
Write-Host "Wrote $out"
Write-Host "Upload that file to Claude."
Write-Host ""
