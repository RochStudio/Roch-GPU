# Read-only AMD streaming PMLog probe. Starts/stops telemetry collection, never tuning.
$ErrorActionPreference = 'Stop'
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class NativeTelemetryProbe {
 public delegate IntPtr Alloc(int size);
 public static Alloc Allocator = n => Marshal.AllocCoTaskMem(n);
 [DllImport("atiadlxx.dll", CallingConvention=CallingConvention.Cdecl)] public static extern int ADL2_Main_Control_Create(Alloc cb,int connected,out IntPtr ctx);
 [DllImport("atiadlxx.dll", CallingConvention=CallingConvention.Cdecl)] public static extern int ADL2_Main_Control_Destroy(IntPtr ctx);
 [DllImport("atiadlxx.dll", CallingConvention=CallingConvention.Cdecl)] public static extern int ADL2_Device_PMLog_Device_Create(IntPtr ctx,int adapter,ref uint device);
 [DllImport("atiadlxx.dll", CallingConvention=CallingConvention.Cdecl)] public static extern int ADL2_Device_PMLog_Device_Destroy(IntPtr ctx,uint device);
 [DllImport("atiadlxx.dll", CallingConvention=CallingConvention.Cdecl)] public static extern int ADL2_Adapter_PMLog_Support_Get(IntPtr ctx,int adapter,IntPtr buffer);
 [DllImport("atiadlxx.dll", CallingConvention=CallingConvention.Cdecl)] public static extern int ADL2_Adapter_PMLog_Start(IntPtr ctx,int adapter,IntPtr input,IntPtr output,uint device);
 [DllImport("atiadlxx.dll", CallingConvention=CallingConvention.Cdecl)] public static extern int ADL2_Adapter_PMLog_Stop(IntPtr ctx,int adapter,uint device);
 [DllImport("atiadlxx.dll", CallingConvention=CallingConvention.Cdecl)] public static extern int ADL2_Overdrive6_CurrentPower_Get(IntPtr ctx,int adapter,int type,ref int value);
 [DllImport("atiadlxx.dll", CallingConvention=CallingConvention.Cdecl)] public static extern int ADL2_OverdriveN_Temperature_Get(IntPtr ctx,int adapter,int type,ref int value);
 [DllImport("atiadlxx.dll", CallingConvention=CallingConvention.Cdecl)] public static extern int ADL2_OverdriveN_PerformanceStatus_Get(IntPtr ctx,int adapter,IntPtr value);
}
'@
$ctx = [IntPtr]::Zero
$device = [uint32]0
$support = [Runtime.InteropServices.Marshal]::AllocHGlobal(576)
$inputBuffer = [Runtime.InteropServices.Marshal]::AllocHGlobal(576)
$outputBuffer = [Runtime.InteropServices.Marshal]::AllocHGlobal(256)
$started = $false
try {
 foreach ($b in @($support,$inputBuffer)) { for ($i=0;$i -lt 576;$i+=4) { [Runtime.InteropServices.Marshal]::WriteInt32($b,$i,0) } }
 for ($i=0;$i -lt 256;$i+=4) { [Runtime.InteropServices.Marshal]::WriteInt32($outputBuffer,$i,0) }
 'Create: ' + [NativeTelemetryProbe]::ADL2_Main_Control_Create([NativeTelemetryProbe]::Allocator,1,[ref]$ctx)
 for ($i=0;$i -lt 4;$i++) {
  $value = -1
  $rc = [NativeTelemetryProbe]::ADL2_Overdrive6_CurrentPower_Get($ctx,0,$i,[ref]$value)
  "OD6 power ${i}: rc=$rc raw=$value watts=$($value/256.0)"
 }
 for ($i=1;$i -le 7;$i++) {
  $value = -1
  $rc = [NativeTelemetryProbe]::ADL2_OverdriveN_Temperature_Get($ctx,0,$i,[ref]$value)
  "ODN temperature ${i}: rc=$rc raw=$value"
 }
 $rc = [NativeTelemetryProbe]::ADL2_OverdriveN_PerformanceStatus_Get($ctx,0,$outputBuffer)
 "ODN performance: rc=$rc"
 if ($rc -eq 0) { for ($i=0;$i -lt 18;$i++) { 'ODN field {0}: {1}' -f $i,[Runtime.InteropServices.Marshal]::ReadInt32($outputBuffer,$i*4) } }
 for ($i=0;$i -lt 256;$i+=4) { [Runtime.InteropServices.Marshal]::WriteInt32($outputBuffer,$i,0) }
 'Support: ' + [NativeTelemetryProbe]::ADL2_Adapter_PMLog_Support_Get($ctx,0,$support)
 $ids = @()
 for ($i=0;$i -lt 255;$i++) {
  $id = [Runtime.InteropServices.Marshal]::ReadInt16($support,$i*2)
  if ($id -eq 0) { break }
  $ids += $id
  [Runtime.InteropServices.Marshal]::WriteInt16($inputBuffer,$i*2,$id)
 }
 'Supported IDs: ' + ($ids -join ', ')
 'Device: ' + [NativeTelemetryProbe]::ADL2_Device_PMLog_Device_Create($ctx,0,[ref]$device)
 [Runtime.InteropServices.Marshal]::WriteInt32($inputBuffer,512,1000)
 $rc = [NativeTelemetryProbe]::ADL2_Adapter_PMLog_Start($ctx,0,$inputBuffer,$outputBuffer,$device)
 'Start: ' + $rc
 $started = $rc -eq 0
 $pointer = [Runtime.InteropServices.Marshal]::ReadIntPtr($outputBuffer)
 if ($started -and $pointer -ne [IntPtr]::Zero) {
  Start-Sleep -Seconds 3
  'Version: ' + [Runtime.InteropServices.Marshal]::ReadInt32($pointer,0)
  for ($i=0;$i -lt 256;$i++) {
   $id = [Runtime.InteropServices.Marshal]::ReadInt32($pointer,16+$i*8)
   if ($id -eq 0) { break }
   '{0}: {1}' -f $id,[Runtime.InteropServices.Marshal]::ReadInt32($pointer,20+$i*8)
  }
 }
} finally {
 if ($started) { [void][NativeTelemetryProbe]::ADL2_Adapter_PMLog_Stop($ctx,0,$device) }
 if ($device -ne 0) { [void][NativeTelemetryProbe]::ADL2_Device_PMLog_Device_Destroy($ctx,$device) }
 if ($ctx -ne [IntPtr]::Zero) { [void][NativeTelemetryProbe]::ADL2_Main_Control_Destroy($ctx) }
 foreach ($b in @($support,$inputBuffer,$outputBuffer)) { [Runtime.InteropServices.Marshal]::FreeHGlobal($b) }
}
