//! Streaming PMLog: the telemetry path for an adapter the one-shot query will not answer.
//!
//! `ADL2_New_QueryPMLogData_Get` hands back an empty buffer for an adapter with no display
//! attached — every sensor marked unsupported — even though the driver streams that same card's
//! sensors quite happily. A second card running headless (a compute card, or one still waiting
//! for a monitor) therefore reads as a column of NaN through the one-shot call alone.
//!
//! So this is the fallback. `ADL2_Adapter_PMLog_Start` asks the driver to refresh a block of
//! `(sensor id, value)` pairs on its own schedule and hands back that block's address; reads are
//! then just volatile loads out of driver-owned memory, with no further ADL call per poll. The
//! session is long-lived: one per card, started on the first poll that needs it and stopped when
//! the backend is disposed.
//!
//! Note the layout difference from the one-shot buffer, which is indexed *by* sensor ID: the
//! streamed block is a packed list terminated by a zero ID, so it is walked, not indexed.

use std::collections::HashMap;
use std::ffi::c_void;
use std::time::{Duration, Instant};

use roch_core::{GpuError, Result};

use crate::adl::{
    ADL_OK, AdlApi, PmLogData, PmLogDeviceDestroy, PmLogStartInput, PmLogStartOutput, PmLogStop, PmLogSupportInfo,
    describe, need,
};

/// How often the driver refreshes the streamed block. The app polls about once a second, and
/// asking for more than that only costs driver work nobody reads.
const SAMPLE_MS: u32 = 1000;

/// How long [`PmLogStream::wait_for_first_sample`] waits for the driver's first write. A little
/// over one sample period: the block is empty until the driver fills it, and a one-shot `sensors`
/// call has nothing to show if it does not wait for it.
const FIRST_SAMPLE_TIMEOUT: Duration = Duration::from_millis(1500);

/// How long each step of that wait sleeps.
const FIRST_SAMPLE_POLL: Duration = Duration::from_millis(25);

/// A running PMLog streaming session for one adapter.
///
/// Dropping it stops the stream and destroys the driver-side device. It borrows nothing: the
/// backend owns one per card alongside the `&mut self` it needs for every other call, so the two
/// entry points it has to keep are held as plain pointers.
pub struct PmLogStream {
    context: *mut c_void,
    adapter: i32,
    device: u32,
    /// Driver-owned block of samples; zero when the stream never produced one.
    address: u64,
    /// Whether `Start` took. The device still needs destroying when it did not.
    started: bool,
    stop: PmLogStop,
    destroy: PmLogDeviceDestroy,
}

// SAFETY: as for the backend that owns it — the ADL context is not thread-affine, and every call
// here goes through `&self` behind the tuning service's single driver lock.
unsafe impl Send for PmLogStream {}

impl PmLogStream {
    /// Ask the driver to stream every sensor `adapter` supports.
    ///
    /// Fails when the driver exports none of the streaming entry points, when the adapter reports
    /// no streamable sensors, or when starting the stream is refused. The caller is expected to
    /// treat a failure as "this card cannot be streamed" and not ask again.
    ///
    /// # Safety
    /// `context` must be a live ADL2 context from `ADL2_Main_Control_Create`, and it must outlive
    /// the stream this returns: stopping the stream calls back into that context on drop.
    pub unsafe fn start(api: &AdlApi, context: *mut c_void, adapter: i32) -> Result<Self> {
        let support_get = need(api.adapter_pm_log_support_get, "ADL2_Adapter_PMLog_Support_Get")?;
        let create = need(api.device_pm_log_device_create, "ADL2_Device_PMLog_Device_Create")?;
        let destroy = need(api.device_pm_log_device_destroy, "ADL2_Device_PMLog_Device_Destroy")?;
        let start = need(api.adapter_pm_log_start, "ADL2_Adapter_PMLog_Start")?;
        let stop = need(api.adapter_pm_log_stop, "ADL2_Adapter_PMLog_Stop")?;

        let mut support = PmLogSupportInfo { ids: [0; 256], reserved: [0; 16] };
        // SAFETY: out-pointer to a local of exactly the size the SDK declares for it.
        let rc = unsafe { support_get(context, adapter, (&mut support as *mut PmLogSupportInfo).cast()) };
        if rc != ADL_OK {
            return Err(GpuError::new(format!("PMLog streaming is unavailable here: {}.", describe(rc))));
        }
        // The driver reads the ID list up to a zero, so keep the last slot zero whatever it filled
        // in: a full 256 IDs would otherwise run it off the end.
        support.ids[255] = 0;
        if support.ids[0] == 0 {
            return Err(GpuError::new("This adapter reports no streamable PMLog sensors."));
        }

        let mut device = 0u32;
        // SAFETY: out-pointer to a local.
        let rc = unsafe { create(context, adapter, &mut device) };
        if rc != ADL_OK {
            return Err(GpuError::new(format!("Could not create a PMLog device: {}.", describe(rc))));
        }
        // Take ownership of the device before anything else can fail: from here on every exit path
        // destroys it through `Drop`.
        let mut stream = Self { context, adapter, device, address: 0, started: false, stop, destroy };

        let mut input = PmLogStartInput { ids: support.ids, sample_ms: SAMPLE_MS, reserved: [0; 15] };
        let mut output = PmLogStartOutput { address: 0, reserved: [0; 14] };
        // SAFETY: both structs are locals matching the SDK layout, and `device` came from Create.
        let rc = unsafe { start(context, adapter, &mut input, &mut output, device) };
        if rc != ADL_OK {
            return Err(GpuError::new(format!("Could not start PMLog streaming: {}.", describe(rc))));
        }
        stream.started = true;
        if output.address == 0 {
            return Err(GpuError::new("PMLog streaming started without a logging address."));
        }
        stream.address = output.address;
        Ok(stream)
    }

    /// The sensors in the driver's latest sample, keyed as the one-shot buffer keys them so the
    /// mapper cannot tell the two apart. Empty until the driver has written its first block.
    pub fn sample(&self) -> HashMap<i32, f64> {
        match self.read_values() {
            Some(values) => parse(&values),
            None => HashMap::new(),
        }
    }

    /// Whether the driver has written anything yet.
    pub fn has_sample(&self) -> bool {
        self.read_values().is_some_and(|values| values[0][0] != 0)
    }

    /// Block until the driver's first sample lands, or [`FIRST_SAMPLE_TIMEOUT`] passes.
    ///
    /// Only worth calling right after [`start`](Self::start): a stream that has been running has a
    /// sample ready, and a card that never produces one must not stall the caller on every poll.
    pub fn wait_for_first_sample(&self) {
        let deadline = Instant::now() + FIRST_SAMPLE_TIMEOUT;
        while !self.has_sample() {
            if Instant::now() >= deadline {
                return;
            }
            std::thread::sleep(FIRST_SAMPLE_POLL);
        }
    }

    /// A copy of the driver's sample array, or `None` when there is no block to read.
    fn read_values(&self) -> Option<[[u32; 2]; 256]> {
        if self.address == 0 {
            return None;
        }
        let data = self.address as *const PmLogData;
        let mut values = [[0u32; 2]; 256];
        for (index, slot) in values.iter_mut().enumerate() {
            // SAFETY: the driver owns this block and keeps it alive until the stream is stopped,
            // which only `Drop` does. Volatile because the driver rewrites it from its own thread
            // while we read, and a raw pointer throughout because the block is never a reference.
            *slot = unsafe { std::ptr::read_volatile(&raw const (*data).values[index]) };
        }
        Some(values)
    }
}

impl Drop for PmLogStream {
    fn drop(&mut self) {
        // SAFETY: `device` came from Device_Create and is stopped (if started) and destroyed once.
        // The backend clears its streams before destroying the ADL context, so `context` is live.
        unsafe {
            if self.started {
                (self.stop)(self.context, self.adapter, self.device);
            }
            (self.destroy)(self.context, self.device);
        }
    }
}

/// The `(id, value)` pairs of a streamed block, up to its zero-ID terminator.
///
/// Values are taken as `i32`, as the one-shot buffer's are, so the mapper's units are unchanged.
fn parse(values: &[[u32; 2]; 256]) -> HashMap<i32, f64> {
    let mut out = HashMap::new();
    for entry in values {
        if entry[0] == 0 {
            break;
        }
        out.insert(entry[0] as i32, entry[1] as i32 as f64);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ptr::null_mut;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn block(pairs: &[(u32, u32)]) -> [[u32; 2]; 256] {
        let mut values = [[0u32; 2]; 256];
        for (slot, (id, value)) in values.iter_mut().zip(pairs) {
            *slot = [*id, *value];
        }
        values
    }

    fn stream_at(address: u64) -> PmLogStream {
        PmLogStream {
            context: null_mut(),
            adapter: 0,
            device: 1,
            address,
            started: false,
            stop: record_stop,
            destroy: record_destroy,
        }
    }

    #[test]
    fn parse_reads_pairs_up_to_the_terminator() {
        let mut values = block(&[(1, 2800), (8, 61), (27, 68)]);
        // Anything past the zero ID is the driver's business, not ours.
        values[4] = [21, 1100];
        let sample = parse(&values);
        assert_eq!(sample.len(), 3);
        assert_eq!(sample[&1], 2800.0);
        assert_eq!(sample[&8], 61.0);
        assert_eq!(sample[&27], 68.0);
        assert!(!sample.contains_key(&21), "entries after the terminator are not read");
    }

    #[test]
    fn parse_of_an_empty_block_is_empty() {
        assert!(parse(&block(&[])).is_empty());
    }

    #[test]
    fn parse_keeps_the_one_shot_signedness() {
        // A sensor the driver fills with a negative reading must not become four billion.
        assert_eq!(parse(&block(&[(74, -5i32 as u32)]))[&74], -5.0);
    }

    static ORDER: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" fn record_stop(_: *mut c_void, _: i32, _: u32) -> i32 {
        ORDER.store(1, Ordering::SeqCst);
        ADL_OK
    }

    unsafe extern "C" fn record_destroy(_: *mut c_void, _: u32) -> i32 {
        let previous = ORDER.load(Ordering::SeqCst);
        ORDER.store(previous * 10 + 2, Ordering::SeqCst);
        ADL_OK
    }

    #[test]
    fn drop_stops_only_a_started_stream_and_always_destroys_the_device() {
        // 2: destroyed without a stop. 12: stopped first, then destroyed.
        for (started, expected) in [(false, 2), (true, 12)] {
            ORDER.store(0, Ordering::SeqCst);
            let mut stream = stream_at(0);
            stream.started = started;
            drop(stream);
            assert_eq!(ORDER.load(Ordering::SeqCst), expected);
        }
    }

    #[test]
    fn a_stream_without_a_block_samples_empty_rather_than_reading_address_zero() {
        let stream = stream_at(0);
        assert!(stream.read_values().is_none());
        assert!(stream.sample().is_empty());
        assert!(!stream.has_sample());
    }

    #[test]
    fn a_stream_samples_the_block_at_its_address() {
        let data = Box::new(PmLogData {
            version: 0,
            sample_ms: SAMPLE_MS,
            timestamp: 0,
            values: block(&[(1, 2795), (8, 54)]),
            reserved: [0; 256],
        });
        let stream = stream_at((&raw const *data) as u64);
        assert!(stream.has_sample());
        let sample = stream.sample();
        assert_eq!(sample[&1], 2795.0);
        assert_eq!(sample[&8], 54.0);
        assert_eq!(sample.len(), 2);
    }
}
