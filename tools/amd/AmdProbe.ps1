# Roch GPU - AMD tuning-surface probe
#
# Read-only: every call here reads. Nothing on the card is changed.
# Writes whats-amd.txt to your Desktop.

$ErrorActionPreference = 'Continue'
$out = Join-Path ([Environment]::GetFolderPath('Desktop')) 'whats-amd.txt'
$lines = New-Object System.Collections.Generic.List[string]
function W($s) { $lines.Add([string]$s) }

W "===== Roch GPU - AMD probe ====="
W (Get-Date).ToString('yyyy-MM-dd HH:mm:ss')
W ""

W "===== 1. adapter (WMI) ====="
try {
    Get-CimInstance Win32_VideoController | ForEach-Object {
        W ("  {0}" -f $_.Name)
        $vram = if ($_.AdapterRAM) { [string][math]::Round($_.AdapterRAM/1MB) + " MB" } else { "?" }
        W ("     driver {0}   date {1}   VRAM {2}" -f $_.DriverVersion, $_.DriverDate, $vram)
    }
} catch { W "  <$($_.Exception.Message)>" }
W ""

W "===== 2. driver libraries ====="
foreach ($dll in @('atiadlxx.dll','amdadlx64.dll','amdadlx32.dll')) {
    $p = Join-Path $env:SystemRoot "System32\$dll"
    if (Test-Path $p) {
        W ("  {0,-16} present   version {1}" -f $dll, (Get-Item $p).VersionInfo.FileVersion)
    } else {
        W ("  {0,-16} MISSING" -f $dll)
    }
}
W ""

$cs = @'
using System;
using System.Runtime.InteropServices;

public static class Adl
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

    // X2/X3 getters let the driver report how many features it has, so nothing here depends on
    // guessing the feature count for this GPU generation.
    [DllImport("atiadlxx.dll", CallingConvention = CallingConvention.Cdecl)]
    public static extern int ADL2_Overdrive8_Init_SettingX2_Get(IntPtr context, int adapterIndex,
        ref int caps, ref int numFeatures, ref IntPtr initSettingList);
    [DllImport("atiadlxx.dll", CallingConvention = CallingConvention.Cdecl)]
    public static extern int ADL2_Overdrive8_Current_SettingX2_Get(IntPtr context, int adapterIndex,
        ref int numFeatures, ref IntPtr currentList);
    [DllImport("atiadlxx.dll", CallingConvention = CallingConvention.Cdecl)]
    public static extern int ADL2_Overdrive8_Current_SettingX3_Get(IntPtr context, int adapterIndex,
        ref int notAdjustableBits, ref int numSettings, ref IntPtr list, int option);
    [DllImport("atiadlxx.dll", CallingConvention = CallingConvention.Cdecl)]
    public static extern int ADL2_Overdrive8_PMLogSenorType_Support_Get(IntPtr context, int adapterIndex,
        ref int numSupported, ref IntPtr typeList);

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
    public static MemAlloc Allocator = new MemAlloc(Alloc);   // field keeps the delegate alive
}

public static class Adlx
{
    [DllImport("amdadlx64.dll", CallingConvention = CallingConvention.Cdecl)]
    public static extern int ADLXQueryFullVersion(out ulong version);
}
'@
Add-Type -TypeDefinition $cs -ErrorAction Stop

W "===== 3. ADLX ====="
try {
    $v = [uint64]0
    $r = [Adlx]::ADLXQueryFullVersion([ref]$v)
    W ("  ADLXQueryFullVersion -> rc={0}  raw=0x{1:X16}  ({2})" -f $r, $v, $v)
} catch { W "  <not callable: $($_.Exception.Message)>" }
W ""

W "===== 4. ADL / Overdrive ====="
$ctx = [IntPtr]::Zero
$rc = -1
try { $rc = [Adl]::ADL2_Main_Control_Create([Adl]::Allocator, 1, [ref]$ctx) } catch { W "  <create failed: $($_.Exception.Message)>" }
W ("  ADL2_Main_Control_Create -> {0}" -f $rc)

# OD8 feature ids, per the ADL SDK enum order. Raw ids are printed too, so a name that does not
# fit its range on this generation is immediately visible.
$od8Names = @(
 'GFXCLK_FMAX','GFXCLK_FMIN','GFXCLK_FREQ1','GFXCLK_VOLTAGE1','GFXCLK_FREQ2','GFXCLK_VOLTAGE2',
 'GFXCLK_FREQ3','GFXCLK_VOLTAGE3','UCLK_FMAX','POWER_PERCENTAGE','FAN_MIN_SPEED','FAN_ACOUSTIC_LIMIT',
 'FAN_TARGET_TEMP','OPERATING_TEMP_MAX','AC_TIMING','FAN_ZERORPM_CONTROL','AUTO_UV_ENGINE_CONTROL',
 'AUTO_OC_ENGINE_CONTROL','AUTO_OC_MEMORY_CONTROL','FAN_CURVE_TEMPERATURE_1','FAN_CURVE_SPEED_1',
 'FAN_CURVE_TEMPERATURE_2','FAN_CURVE_SPEED_2','FAN_CURVE_TEMPERATURE_3','FAN_CURVE_SPEED_3',
 'FAN_CURVE_TEMPERATURE_4','FAN_CURVE_SPEED_4','FAN_CURVE_TEMPERATURE_5','FAN_CURVE_SPEED_5',
 'UCLK_FMIN','POWER_TDC_LIMIT','FULL_CONTROL_MODE')

if ($rc -eq 0 -and $ctx -ne [IntPtr]::Zero) {
    $num = 0
    $null = [Adl]::ADL2_Adapter_NumberOfAdapters_Get($ctx, [ref]$num)
    W ("  adapters reported: {0}" -f $num)

    if ($num -gt 0) {
        $sz = [System.Runtime.InteropServices.Marshal]::SizeOf([type][Adl+AdapterInfo])
        $buf = [System.Runtime.InteropServices.Marshal]::AllocHGlobal($sz * $num)
        try {
            $r = [Adl]::ADL2_Adapter_AdapterInfo_Get($ctx, $buf, $sz * $num)
            W ("  ADL2_Adapter_AdapterInfo_Get -> {0}   (struct size {1})" -f $r, $sz)

            $seen = @{}
            for ($i = 0; $i -lt $num; $i++) {
                $p = [IntPtr]::Add($buf, $i * $sz)
                $ai = [System.Runtime.InteropServices.Marshal]::PtrToStructure($p, [type][Adl+AdapterInfo])
                if ($ai.iVendorID -ne 1002) { continue }
                $key = "$($ai.iBusNumber).$($ai.iDeviceNumber).$($ai.iFunctionNumber)"
                if ($seen.ContainsKey($key)) { continue }
                $seen[$key] = $true

                W ""
                W ("  --- adapter index {0}  bus {1} dev {2} fn {3} ---" -f $ai.iAdapterIndex, $ai.iBusNumber, $ai.iDeviceNumber, $ai.iFunctionNumber)
                W ("      name    : {0}" -f $ai.strAdapterName)
                W ("      pnp     : {0}" -f $ai.strPNPString)
                W ("      present : {0}   exist {1}" -f $ai.iPresent, $ai.iExist)

                $sup = 0; $en = 0; $ver = 0
                $r = [Adl]::ADL2_Overdrive_Caps($ctx, $ai.iAdapterIndex, [ref]$sup, [ref]$en, [ref]$ver)
                W ("      ADL2_Overdrive_Caps -> rc={0}  supported={1} enabled={2} VERSION={3}" -f $r,$sup,$en,$ver)

                $caps = 0; $nf = 0; $listPtr = [IntPtr]::Zero
                $r = [Adl]::ADL2_Overdrive8_Init_SettingX2_Get($ctx, $ai.iAdapterIndex, [ref]$caps, [ref]$nf, [ref]$listPtr)
                W ("      OD8 Init_SettingX2 -> rc={0}  capsBits=0x{1:X8}  features={2}" -f $r, $caps, $nf)
                if ($r -eq 0 -and $nf -gt 0 -and $listPtr -ne [IntPtr]::Zero) {
                    for ($k = 0; $k -lt $nf; $k++) {
                        $e = [IntPtr]::Add($listPtr, $k * 16)
                        $fid = [System.Runtime.InteropServices.Marshal]::ReadInt32($e, 0)
                        $mn  = [System.Runtime.InteropServices.Marshal]::ReadInt32($e, 4)
                        $mx  = [System.Runtime.InteropServices.Marshal]::ReadInt32($e, 8)
                        $df  = [System.Runtime.InteropServices.Marshal]::ReadInt32($e, 12)
                        $nm = if ($fid -ge 0 -and $fid -lt $od8Names.Count) { $od8Names[$fid] } else { "id$fid" }
                        W ("        [{0,2}] {1,-24} min={2,-8} max={3,-8} default={4}" -f $fid, $nm, $mn, $mx, $df)
                    }
                }

                $nc = 0; $curPtr = [IntPtr]::Zero
                $r = [Adl]::ADL2_Overdrive8_Current_SettingX2_Get($ctx, $ai.iAdapterIndex, [ref]$nc, [ref]$curPtr)
                W ("      OD8 Current_SettingX2 -> rc={0}  count={1}" -f $r, $nc)
                if ($r -eq 0 -and $nc -gt 0 -and $curPtr -ne [IntPtr]::Zero) {
                    for ($k = 0; $k -lt $nc; $k++) {
                        $val = [System.Runtime.InteropServices.Marshal]::ReadInt32($curPtr, $k * 4)
                        $nm = if ($k -lt $od8Names.Count) { $od8Names[$k] } else { "id$k" }
                        W ("        [{0,2}] {1,-24} = {2}" -f $k, $nm, $val)
                    }
                }

                $nadj = 0; $ns = 0; $x3 = [IntPtr]::Zero
                $r = [Adl]::ADL2_Overdrive8_Current_SettingX3_Get($ctx, $ai.iAdapterIndex, [ref]$nadj, [ref]$ns, [ref]$x3, 0)
                W ("      OD8 Current_SettingX3 -> rc={0}  notAdjustableBits=0x{1:X8}  count={2}" -f $r, $nadj, $ns)

                $nst = 0; $stPtr = [IntPtr]::Zero
                $r = [Adl]::ADL2_Overdrive8_PMLogSenorType_Support_Get($ctx, $ai.iAdapterIndex, [ref]$nst, [ref]$stPtr)
                W ("      PMLog sensor types -> rc={0}  count={1}" -f $r, $nst)
                if ($r -eq 0 -and $nst -gt 0 -and $stPtr -ne [IntPtr]::Zero) {
                    $ids = @()
                    for ($k = 0; $k -lt $nst; $k++) { $ids += [System.Runtime.InteropServices.Marshal]::ReadInt32($stPtr, $k * 4) }
                    W ("        sensor ids: {0}" -f ($ids -join ', '))
                }
            }
        } finally {
            [System.Runtime.InteropServices.Marshal]::FreeHGlobal($buf)
        }
    }
    $null = [Adl]::ADL2_Main_Control_Destroy($ctx)
}

W ""
W "===== end ====="
Set-Content -Path $out -Value $lines -Encoding UTF8
Write-Host ""
Write-Host "Wrote $out"
Write-Host "Upload that file to Claude."
Write-Host ""
