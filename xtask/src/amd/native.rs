//! ADL prototypes reuse the production binding; streaming layouts follow AMD's adl_structures.h.
use std::ffi::c_void;
use std::fmt::Write;
use std::mem::ManuallyDrop;
use std::ptr::{null_mut, read_unaligned};

use super::{Adapter, Driver, Feature};
use crate::Result;
use roch_amd::adl::{self, AdapterInfo, AdlApi, OD8_COUNT, need};
use roch_core::native::{self, Library};

#[path = "names.rs"]
mod names;
pub fn feature_name(id: usize) -> String {
    names::FEATURE_NAMES.get(id).map_or_else(|| format!("id{id}"), |s| (*s).to_owned())
}
pub fn system32() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var_os("WINDIR").unwrap_or_else(|| r"C:\Windows".into())).join("System32")
}
fn checked(rc: i32, name: &str) -> Result<()> {
    if rc == 0 { Ok(()) } else { Err(format!("{name}: {} ({rc})", adl::describe(rc))) }
}
struct OwnedList(*mut c_void);
impl Drop for OwnedList {
    fn drop(&mut self) {
        unsafe {
            adl::co_free(self.0);
        }
    }
}
fn count(value: i32) -> Result<usize> {
    if (0..=4096).contains(&value) { Ok(value as usize) } else { Err(format!("Invalid driver table count: {value}")) }
}

pub struct Session {
    api: ManuallyDrop<AdlApi>,
    lib: ManuallyDrop<Library>,
    context: *mut c_void,
}
impl Session {
    pub fn new() -> Result<Self> {
        let api = AdlApi::load().map_err(|e| e.to_string())?;
        need(api.main_control_destroy, "ADL2_Main_Control_Destroy").map_err(|e| e.to_string())?;
        let lib = native::load_system32(adl::DLL).map_err(|e| e.to_string())?;
        let mut session = Self { api: ManuallyDrop::new(api), lib: ManuallyDrop::new(lib), context: null_mut() };
        // SAFETY: production ADL prototypes, a static allocator, and a live output pointer.
        checked(unsafe { (session.api.main_control_create)(adl::ALLOCATOR, 1, &mut session.context) }, "ADL create")?;
        if session.context.is_null() {
            return Err("ADL returned a null context".into());
        }
        Ok(session)
    }
    unsafe fn function<F: Copy>(&self, name: &str) -> Result<F> {
        unsafe { native::symbol(&self.lib, name) }.map_err(|e| e.to_string())
    }
    fn current_values(&self, adapter: i32) -> Result<Vec<i32>> {
        let get = need(self.api.od8_current_setting_x2_get, "Current_SettingX2").map_err(|e| e.to_string())?;
        let (mut length, mut pointer) = (75, null_mut());
        // SAFETY: driver allocates the output through our CoTaskMem allocator.
        let rc = unsafe { get(self.context, adapter, &mut length, &mut pointer) };
        let owned = OwnedList(pointer);
        checked(rc, "Current_SettingX2")?;
        let length = count(length)?;
        if pointer.is_null() || length == 0 {
            return Err("No current OD8 values returned".into());
        }
        let values = (0..length).map(|i| unsafe { read_unaligned(owned.0.cast::<i32>().add(i)) }).collect();
        Ok(values)
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        if !self.context.is_null()
            && let Some(destroy) = self.api.main_control_destroy
        {
            unsafe {
                destroy(self.context);
            }
        }
        // As in the production backend, leave driver DLLs mapped: worker threads can outlive Destroy.
    }
}

impl Driver for Session {
    fn adapters(&mut self) -> Result<Vec<Adapter>> {
        let get_count = need(self.api.number_of_adapters_get, "NumberOfAdapters_Get").map_err(|e| e.to_string())?;
        let get_info = need(self.api.adapter_info_get, "AdapterInfo_Get").map_err(|e| e.to_string())?;
        let mut length = 0;
        checked(unsafe { get_count(self.context, &mut length) }, "NumberOfAdapters_Get")?;
        let length = count(length)?;
        let mut info = vec![AdapterInfo::zeroed(); length];
        for entry in &mut info {
            entry.i_size = std::mem::size_of::<AdapterInfo>() as i32;
        }
        if length > 0 {
            checked(
                unsafe {
                    get_info(self.context, info.as_mut_ptr().cast(), std::mem::size_of_val(info.as_slice()) as i32)
                },
                "AdapterInfo_Get",
            )?;
        }
        Ok(info
            .into_iter()
            .filter(|a| a.i_vendor_id == adl::AMD_VENDOR_ID)
            .map(|a| Adapter {
                index: a.i_adapter_index,
                name: format!(
                    "{} (bus {}, device {}, function {}; {})",
                    native::c_str(&a.str_adapter_name),
                    a.i_bus_number,
                    a.i_device_number,
                    a.i_function_number,
                    native::c_str(&a.str_pnp_string)
                ),
            })
            .collect())
    }
    fn details(&mut self, adapter: i32) -> Result<String> {
        type X3 = unsafe extern "C" fn(*mut c_void, i32, *mut i32, *mut i32, *mut *mut c_void, i32) -> i32;
        type SensorTypes = unsafe extern "C" fn(*mut c_void, i32, *mut i32, *mut *mut c_void) -> i32;
        type Buffer = unsafe extern "C" fn(*mut c_void, i32, *mut c_void) -> i32;
        type Version = unsafe extern "C" fn(*mut u64) -> i32;
        let mut text = String::new();
        for (get, name) in [
            (self.api.graphics_versions_x2_get, "Graphics_VersionsX2"),
            (self.api.graphics_versions_get, "Graphics_Versions"),
        ] {
            if let Some(get) = get {
                let mut buffer = [0u8; 4096];
                let rc = unsafe { get(self.context, buffer.as_mut_ptr().cast()) };
                let strings: Vec<_> =
                    buffer.split(|b| *b == 0).filter(|s| !s.is_empty()).map(String::from_utf8_lossy).collect();
                writeln!(text, "{name}: rc={rc} {}", strings.join(" | ")).unwrap();
            }
        }
        if let Some(get) = self.api.overdrive_caps {
            let (mut supported, mut enabled, mut version) = (0, 0, 0);
            let rc = unsafe { get(self.context, adapter, &mut supported, &mut enabled, &mut version) };
            writeln!(text, "Overdrive: rc={rc} supported={supported} enabled={enabled} version={version}").unwrap();
        }
        if let Ok(lib) = native::load_system32("amdadlx64.dll") {
            if let Ok(get) = unsafe { native::symbol::<Version>(&lib, "ADLXQueryFullVersion") } {
                let mut value = 0;
                let rc = unsafe { get(&mut value) };
                writeln!(text, "ADLX full version: rc={rc}, 0x{value:016X}").unwrap();
            }
            std::mem::forget(lib);
        }
        if let Ok(get) = unsafe { self.function::<X3>("ADL2_Overdrive8_Current_SettingX3_Get") } {
            let (mut bits, mut length, mut pointer) = (0, 75, null_mut());
            let rc = unsafe { get(self.context, adapter, &mut bits, &mut length, &mut pointer, 0) };
            let _owned = OwnedList(pointer);
            writeln!(text, "Current_SettingX3: rc={rc}, not_adjustable=0x{bits:08X}, count={length}").unwrap();
        }
        if let Ok(get) = unsafe { self.function::<SensorTypes>("ADL2_Overdrive8_PMLogSenorType_Support_Get") } {
            let (mut length, mut pointer) = (0, null_mut());
            let rc = unsafe { get(self.context, adapter, &mut length, &mut pointer) };
            let owned = OwnedList(pointer);
            writeln!(text, "PMLog sensor types: rc={rc}, count={length}").unwrap();
            if rc == 0 && !owned.0.is_null() {
                for i in 0..count(length)? {
                    write!(text, "{} ", unsafe { read_unaligned(owned.0.cast::<i32>().add(i)) }).unwrap();
                }
                text.push('\n');
            }
        }
        for (name, header, stride) in
            [("ADL2_Overdrive8_Init_Setting_Get", 2usize, 4usize), ("ADL2_Overdrive8_Current_Setting_Get", 1, 1)]
        {
            if let Ok(get) = unsafe { self.function::<Buffer>(name) } {
                let mut buffer = vec![0i32; 16_384];
                let rc = unsafe { get(self.context, adapter, buffer.as_mut_ptr().cast()) };
                writeln!(text, "{name}: rc={rc}, count={}", buffer[0]).unwrap();
                if rc == 0 {
                    let length = count(buffer[0])?;
                    if length * stride + header <= buffer.len() {
                        for i in 0..length {
                            writeln!(text, "  [{i}] {:?}", &buffer[header + i * stride..header + (i + 1) * stride])
                                .unwrap();
                        }
                    }
                }
            }
        }
        Ok(text)
    }
    fn features(&mut self, adapter: i32, seed: i32) -> Result<(i32, Vec<Feature>)> {
        let get = need(self.api.od8_init_setting_x2_get, "Init_SettingX2").map_err(|e| e.to_string())?;
        let (mut caps, mut length, mut pointer) = (0, seed, null_mut());
        let rc = unsafe { get(self.context, adapter, &mut caps, &mut length, &mut pointer) };
        let owned = OwnedList(pointer);
        checked(rc, "Init_SettingX2")?;
        let length = count(length)?;
        if owned.0.is_null() && length != 0 {
            return Err("Null OD8 feature table".into());
        }
        let mut features = Vec::with_capacity(length);
        for i in 0..length {
            // SAFETY: each driver-allocated ADLOD8SingleInitSetting has four i32 fields.
            let at = unsafe { owned.0.cast::<i32>().add(i * 4) };
            features.push(Feature {
                id: i,
                minimum: unsafe { read_unaligned(at.add(1)) },
                maximum: unsafe { read_unaligned(at.add(2)) },
                default: unsafe { read_unaligned(at.add(3)) },
            });
        }
        Ok((caps, features))
    }
    fn current(&mut self, adapter: i32) -> Result<Vec<i32>> {
        self.current_values(adapter)
    }
    fn sensors(&mut self, adapter: i32) -> Result<Vec<(usize, i32)>> {
        let get = need(self.api.new_query_pm_log_data_get, "New_QueryPMLogData_Get").map_err(|e| e.to_string())?;
        let mut buffer = [0i32; 1 + 256 * 2];
        checked(unsafe { get(self.context, adapter, buffer.as_mut_ptr().cast()) }, "New_QueryPMLogData_Get")?;
        Ok((0..256).filter(|i| buffer[1 + i * 2] != 0).map(|i| (i, buffer[2 + i * 2])).collect())
    }
    fn write(&mut self, adapter: i32, pairs: &[(usize, i32)], count: i32) -> Result<()> {
        let get = need(self.api.od8_setting_set, "Setting_Set").map_err(|e| e.to_string())?;
        if ![75, 77].contains(&count) || pairs.iter().any(|(id, _)| *id >= OD8_COUNT) {
            return Err("Invalid OD8 write shape".into());
        }
        let mut set = [0i32; 1 + OD8_COUNT * 3];
        let mut current = [0i32; 1 + OD8_COUNT];
        set[0] = count;
        current[0] = count;
        for &(id, value) in pairs {
            set[1 + id * 3] = value;
            set[2 + id * 3] = 1;
        }
        checked(
            unsafe { get(self.context, adapter, set.as_mut_ptr().cast(), current.as_mut_ptr().cast()) },
            "Setting_Set",
        )
    }
    fn stream(&mut self, adapter: i32) -> Result<String> {
        stream(self, adapter)
    }
    fn wait(&self, milliseconds: u64) {
        std::thread::sleep(std::time::Duration::from_millis(milliseconds));
    }
}

#[repr(C)]
struct Support {
    ids: [u16; 256],
    reserved: [i32; 16],
}
#[repr(C)]
struct Input {
    ids: [u16; 256],
    sample_ms: u32,
    reserved: [i32; 15],
}
#[repr(C)]
struct Output {
    address: u64,
    reserved: [i32; 14],
}
#[repr(C)]
struct Data {
    version: u32,
    sample_ms: u32,
    timestamp: u64,
    values: [[u32; 2]; 256],
    reserved: [u32; 256],
}
type DeviceCreate = unsafe extern "C" fn(*mut c_void, i32, *mut u32) -> i32;
type DeviceDestroy = unsafe extern "C" fn(*mut c_void, u32) -> i32;
type Stop = unsafe extern "C" fn(*mut c_void, i32, u32) -> i32;
struct Stream<'a> {
    context: *mut c_void,
    _session: std::marker::PhantomData<&'a Session>,
    adapter: i32,
    device: u32,
    started: bool,
    stop: Stop,
    destroy: DeviceDestroy,
}
impl Drop for Stream<'_> {
    fn drop(&mut self) {
        unsafe {
            if self.started {
                (self.stop)(self.context, self.adapter, self.device);
            }
            (self.destroy)(self.context, self.device);
        }
    }
}
impl<'a> Stream<'a> {
    fn new(session: &'a Session, adapter: i32, device: u32, stop: Stop, destroy: DeviceDestroy) -> Self {
        Self {
            context: session.context,
            _session: std::marker::PhantomData,
            adapter,
            device,
            started: false,
            stop,
            destroy,
        }
    }
}

fn stream(session: &Session, adapter: i32) -> Result<String> {
    type Value = unsafe extern "C" fn(*mut c_void, i32, i32, *mut i32) -> i32;
    type Buffer = unsafe extern "C" fn(*mut c_void, i32, *mut c_void) -> i32;
    type Start = unsafe extern "C" fn(*mut c_void, i32, *mut Input, *mut Output, u32) -> i32;
    let mut text = String::new();
    for (name, range) in [("ADL2_Overdrive6_CurrentPower_Get", 0..4), ("ADL2_OverdriveN_Temperature_Get", 1..8)] {
        if let Ok(get) = unsafe { session.function::<Value>(name) } {
            for kind in range {
                let mut value = -1;
                let rc = unsafe { get(session.context, adapter, kind, &mut value) };
                writeln!(text, "{name} {kind}: rc={rc} raw={value}").unwrap();
            }
        }
    }
    if let Ok(get) = unsafe { session.function::<Buffer>("ADL2_OverdriveN_PerformanceStatus_Get") } {
        let mut buffer = [0i32; 64];
        let rc = unsafe { get(session.context, adapter, buffer.as_mut_ptr().cast()) };
        writeln!(text, "ODN performance: rc={rc} {:?}", &buffer[..18]).unwrap();
    }
    let (support_get, create, destroy, start, stop) = unsafe {
        (
            session.function::<Buffer>("ADL2_Adapter_PMLog_Support_Get")?,
            session.function::<DeviceCreate>("ADL2_Device_PMLog_Device_Create")?,
            session.function::<DeviceDestroy>("ADL2_Device_PMLog_Device_Destroy")?,
            session.function::<Start>("ADL2_Adapter_PMLog_Start")?,
            session.function::<Stop>("ADL2_Adapter_PMLog_Stop")?,
        )
    };
    let mut support = Support { ids: [0; 256], reserved: [0; 16] };
    checked(
        unsafe { support_get(session.context, adapter, (&mut support as *mut Support).cast()) },
        "PMLog_Support_Get",
    )?;
    // Leave the last ID zero to guarantee the SDK's terminating sentinel.
    support.ids[255] = 0;
    let mut input = Input { ids: support.ids, sample_ms: 1000, reserved: [0; 15] };
    let mut output = Output { address: 0, reserved: [0; 14] };
    let mut device = 0;
    checked(unsafe { create(session.context, adapter, &mut device) }, "PMLog_Device_Create")?;
    let mut guard = Stream::new(session, adapter, device, stop, destroy);
    checked(unsafe { start(session.context, adapter, &mut input, &mut output, device) }, "PMLog_Start")?;
    guard.started = true;
    if output.address == 0 {
        return Err("PMLog returned no logging address".into());
    }
    std::thread::sleep(std::time::Duration::from_secs(3));
    // SAFETY: driver-owned streaming memory remains alive until guard stops/destroys this device.
    let data = output.address as *const Data;
    let version = unsafe { std::ptr::read_volatile(std::ptr::addr_of!((*data).version)) };
    writeln!(text, "Streaming version={version}").unwrap();
    for index in 0..256 {
        let value = unsafe { std::ptr::read_volatile(std::ptr::addr_of!((*data).values[index])) };
        if value[0] == 0 {
            break;
        }
        writeln!(text, "sensor[{}] = {}", value[0], value[1]).unwrap();
    }
    drop(guard);
    text.push_str("Telemetry collection stopped; device destroyed.\n");
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn streaming_cleanup_stops_only_started_streams_and_always_destroys_the_device() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static ORDER: AtomicUsize = AtomicUsize::new(0);
        unsafe extern "C" fn stop(_: *mut c_void, _: i32, _: u32) -> i32 {
            ORDER.store(1, Ordering::SeqCst);
            0
        }
        unsafe extern "C" fn destroy(_: *mut c_void, _: u32) -> i32 {
            let previous = ORDER.load(Ordering::SeqCst);
            ORDER.store(previous * 10 + 2, Ordering::SeqCst);
            0
        }
        for (started, expected) in [(false, 2), (true, 12)] {
            ORDER.store(0, Ordering::SeqCst);
            drop(Stream {
                context: null_mut(),
                _session: std::marker::PhantomData,
                adapter: 0,
                device: 1,
                started,
                stop,
                destroy,
            });
            assert_eq!(ORDER.load(Ordering::SeqCst), expected);
        }
    }
    #[test]
    fn sdk_streaming_layouts_and_counts_match_the_windows_abi() {
        assert_eq!(std::mem::size_of::<Support>(), 576);
        assert_eq!(std::mem::size_of::<Input>(), 576);
        assert_eq!(std::mem::size_of::<Output>(), 64);
        assert_eq!(std::mem::offset_of!(Data, values), 16);
        assert_eq!(feature_name(37), "OD_VOLTAGE");
        assert!(count(-1).is_err());
        assert!(count(i32::MAX).is_err());
    }
}
