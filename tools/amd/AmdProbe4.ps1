# ROCH GPU OC - AMD probe 4: does the GFX clock offset actually stick?
#
# Writes GFXCLK_FMAX = +200, reads it back, and restores it to 0. Four different write shapes are
# tried; the first that reads back correctly wins. Also samples the live core clock either side, so
# "the driver took it but the card is power-bound" is distinguishable from "the driver refused it".
#
# This DOES write to the GPU, but only the core clock offset, and it puts it back to 0 at the end.
# Writes whats-amd4.txt to your Desktop.

$ErrorActionPreference = 'Continue'
$out = Join-Path ([Environment]::GetFolderPath('Desktop')) 'whats-amd4.txt'
$lines = New-Object System.Collections.Generic.List[string]
function W($s) { $lines.Add([string]$s); Write-Host $s }

$OD8_COUNT = 77
$NFEAT     = 75
$ID_FMAX   = 0
$ID_FMIN   = 1
$ID_UCLK   = 8
$ID_VOLT   = 37

W "===== ROCH GPU OC - AMD probe 4 (core clock offset) ====="
W (Get-Date).ToString('yyyy-MM-dd HH:mm:ss')
W ""

$cs = @'
using System;
using System.Runtime.InteropServices;

public static class Adl4
{
    public delegate IntPtr MemAlloc(int size);
    [DllImport("atiadlxx.dll", CallingConvention = CallingConvention.Cdecl)]
    public static extern int ADL2_Main_Control_Create(MemAlloc cb, int enumConnected, out IntPtr context);
    [DllImport("atiadlxx.dll", CallingConvention = CallingConvention.Cdecl)]
    public static extern int ADL2_Main_Control_Destroy(IntPtr context);
    [DllImport("atiadlxx.dll", CallingConvention = CallingConvention.Cdecl)]
    public static extern int ADL2_Overdrive8_Current_SettingX2_Get(IntPtr context, int adapterIndex,
        ref int numFeatures, ref IntPtr list);
    [DllImport("atiadlxx.dll", CallingConvention = CallingConvention.Cdecl)]
    public static extern int ADL2_Overdrive8_Setting_Set(IntPtr context, int adapterIndex,
        IntPtr setSetting, IntPtr currentSetting);
    [DllImport("atiadlxx.dll", CallingConvention = CallingConvention.Cdecl)]
    public static extern int ADL2_New_QueryPMLogData_Get(IntPtr context, int adapterIndex, IntPtr dataOutput);
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
$rc = [Adl4]::ADL2_Main_Control_Create([Adl4]::Allocator, 1, [ref]$ctx)
W ("ADL init -> {0}" -f (Rc $rc))
if ($rc -ne 0) { Set-Content $out $lines; exit 1 }

function Read-All {
    $n = $NFEAT; $p = [IntPtr]::Zero
    $r = [Adl4]::ADL2_Overdrive8_Current_SettingX2_Get($ctx, 0, [ref]$n, [ref]$p)
    if ($r -ne 0 -or $p -eq [IntPtr]::Zero) { return $null }
    $a = New-Object int[] $OD8_COUNT
    for ($i = 0; $i -lt [Math]::Min($n, $OD8_COUNT); $i++) {
        $a[$i] = [System.Runtime.InteropServices.Marshal]::ReadInt32($p, $i * 4)
    }
    return $a
}

function Read-CoreClock {
    $size = 4 + 256 * 8
    $b = [System.Runtime.InteropServices.Marshal]::AllocHGlobal($size)
    try {
        for ($i = 0; $i -lt $size; $i += 4) { [System.Runtime.InteropServices.Marshal]::WriteInt32($b, $i, 0) }
        if ([Adl4]::ADL2_New_QueryPMLogData_Get($ctx, 0, $b) -ne 0) { return -1 }
        $o = 4 + 1 * 8      # PMLOG_CLK_GFXCLK = 1
        if ([System.Runtime.InteropServices.Marshal]::ReadInt32($b, $o) -eq 0) { return -1 }
        return [System.Runtime.InteropServices.Marshal]::ReadInt32($b, $o + 4)
    } finally { [System.Runtime.InteropServices.Marshal]::FreeHGlobal($b) }
}

# $pairs = array of @(id, value); $count = what to put in the struct's count field
function Write-Od8($pairs, $count, $resetFlag) {
    $setSize = 4 + $OD8_COUNT * 12
    $curSize = 4 + $OD8_COUNT * 4
    $set = [System.Runtime.InteropServices.Marshal]::AllocHGlobal($setSize)
    $cur = [System.Runtime.InteropServices.Marshal]::AllocHGlobal($curSize)
    try {
        for ($i = 0; $i -lt $setSize; $i += 4) { [System.Runtime.InteropServices.Marshal]::WriteInt32($set, $i, 0) }
        for ($i = 0; $i -lt $curSize; $i += 4) { [System.Runtime.InteropServices.Marshal]::WriteInt32($cur, $i, 0) }
        [System.Runtime.InteropServices.Marshal]::WriteInt32($set, 0, $count)
        [System.Runtime.InteropServices.Marshal]::WriteInt32($cur, 0, $count)
        foreach ($p in $pairs) {
            $o = 4 + $p[0] * 12
            [System.Runtime.InteropServices.Marshal]::WriteInt32($set, $o, $p[1])        # value
            [System.Runtime.InteropServices.Marshal]::WriteInt32($set, $o + 4, 1)        # requested
            [System.Runtime.InteropServices.Marshal]::WriteInt32($set, $o + 8, $resetFlag)
        }
        return [Adl4]::ADL2_Overdrive8_Setting_Set($ctx, 0, $set, $cur)
    } finally {
        [System.Runtime.InteropServices.Marshal]::FreeHGlobal($set)
        [System.Runtime.InteropServices.Marshal]::FreeHGlobal($cur)
    }
}

$before = Read-All
W ""
W "----- before -----"
W ("  GFXCLK_FMAX = {0}    UCLK_FMAX = {1}    OD_VOLTAGE = {2}" -f $before[$ID_FMAX], $before[$ID_UCLK], $before[$ID_VOLT])
W ("  live core clock = {0} MHz" -f (Read-CoreClock))
W ""

$target = 200
$recipes = @(
  @{ n = "1. FMAX alone, count=75";                 pairs = @(,@($ID_FMAX, $target));                            count = 75; reset = 0 },
  @{ n = "2. FMAX alone, count=77";                 pairs = @(,@($ID_FMAX, $target));                            count = 77; reset = 0 },
  @{ n = "3. FMAX + FMIN(0), count=75";             pairs = @(@($ID_FMAX, $target), @($ID_FMIN, 0));             count = 75; reset = 0 },
  @{ n = "4. FMAX + UCLK + VOLT together, count=75"; pairs = @(@($ID_FMAX, $target), @($ID_UCLK, $before[$ID_UCLK]), @($ID_VOLT, $before[$ID_VOLT])); count = 75; reset = 0 }
)

$winner = $null
foreach ($r in $recipes) {
    $rcw = Write-Od8 $r.pairs $r.count $r.reset
    Start-Sleep -Milliseconds 400
    $after = Read-All
    $got = if ($after) { $after[$ID_FMAX] } else { 'read failed' }
    $ok = ($after -ne $null -and $after[$ID_FMAX] -eq $target)
    W ("{0,-42} set->{1,-14} reads back {2}   {3}" -f $r.n, (Rc $rcw), $got, $(if ($ok) { '<<< STICKS' } else { '' }))
    if ($ok -and -not $winner) { $winner = $r.n }
    # put it back before trying the next shape
    $null = Write-Od8 @(,@($ID_FMAX, 0)) 75 0
    Start-Sleep -Milliseconds 300
}

W ""
if ($winner) {
    W "RESULT: the driver accepts the core clock offset via -> $winner"
    W "Re-applying +200 and sampling the live clock for 5 seconds..."
    $null = Write-Od8 @(,@($ID_FMAX, $target)) 75 0
    Start-Sleep -Milliseconds 500
    $samples = @()
    for ($i = 0; $i -lt 5; $i++) { $samples += (Read-CoreClock); Start-Sleep -Milliseconds 700 }
    W ("  core clock with +200 applied: {0} MHz" -f ($samples -join ', '))
    W "  (at idle this barely moves - a max-frequency offset only shows under load)"
    $null = Write-Od8 @(,@($ID_FMAX, 0)) 75 0
    W "  restored to 0."
} else {
    W "RESULT: none of the four write shapes stuck. The driver is refusing the GFX clock offset"
    W "        while accepting memory, power and voltage writes through the identical call."
}

$final = Read-All
W ""
W ("----- after (restored) -----")
W ("  GFXCLK_FMAX = {0}    UCLK_FMAX = {1}    OD_VOLTAGE = {2}" -f $final[$ID_FMAX], $final[$ID_UCLK], $final[$ID_VOLT])

$null = [Adl4]::ADL2_Main_Control_Destroy($ctx)
Set-Content -Path $out -Value $lines -Encoding UTF8
Write-Host ""
Write-Host "Wrote $out"
