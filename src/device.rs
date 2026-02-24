/// Changelog:
/// - use Bytes instead of Vec<u8>
/// - use bounded channels instead of unbounded
/// - make MTU variable
///
///
// TODO: Try with unbounded channels? Fix smoltcp stuff... Also check batching to push incoming and how to drain all outgoing quickly!

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use smoltcp::{
    phy::{Device, DeviceCapabilities, Medium, RxToken, TxToken},
    time::Instant,
};
use tokio::sync::mpsc::{Permit, Receiver, Sender, channel};
use tokio_util::bytes::Bytes;
use tracing::{info, trace, warn};

use crate::{BufferPool, packet::AnyIpPktFrame};

// #region agent log
//#[inline(never)]
fn debug_log(location: &str, message: &str, hypothesis_id: &str, data: &str) {
    info!(
        r#"{{"sessionId":"4abf9e","location":"{}","message":"{}","hypothesisId":"{}","data":"{}","timestamp":{}}}\n"#,
        location,
        message,
        hypothesis_id,
        data.replace('\\', "\\\\").replace('"', "\\\""),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    );
}
// #endregion

pub(super) struct VirtualDevice {
    in_buf_avail: Arc<AtomicBool>,
    in_buf: Receiver<Bytes>,
    out_buf: Sender<AnyIpPktFrame>,
    max_transmission_unit: usize,
    buffer_pool: BufferPool,
}

impl VirtualDevice {
    pub(super) fn new(
        iface_egress_tx: Sender<AnyIpPktFrame>,
        max_transmission_unit: usize,
    ) -> (Self, Sender<Bytes>, Arc<AtomicBool>) {
        let iface_ingress_tx_avail = Arc::new(AtomicBool::new(false));
        // Ingress queue: packets from pipeline → smoltcp. Large enough to absorb bursts (e.g. speedtest).
        let (iface_ingress_tx, iface_ingress_rx) = channel(1024);
        (
            Self {
                in_buf_avail: iface_ingress_tx_avail.clone(),
                in_buf: iface_ingress_rx,
                out_buf: iface_egress_tx,
                max_transmission_unit,
                buffer_pool: BufferPool::new(64 * 1024),
            },
            iface_ingress_tx,
            iface_ingress_tx_avail,
        )
    }
}

impl Device for VirtualDevice {
    type RxToken<'a> = VirtualRxToken;
    type TxToken<'a> = VirtualTxToken<'a>;

    fn receive(&mut self, _timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        // #region agent log
        //trace!("[SmlDev] RX out_capacity: {}", self.out_buf.capacity());
        let Ok(permit) = self.out_buf.try_reserve() else {
            // debug_log(
            //     "device.rs:receive",
            //     "receive None",
            //     "A",
            //     "egress_full_try_reserve",
            // );
            // info!("RX NO TX");
            self.in_buf_avail.store(false, Ordering::Release);
            return None;
        };
        let Ok(buffer) = self.in_buf.try_recv() else {
            // debug_log(
            //     "device.rs:receive",
            //     "receive None",
            //     "C",
            //     "no_packet_try_recv",
            // );
            self.in_buf_avail.store(false, Ordering::Release);
            return None;
        };
        // #endregion

        Some((
            Self::RxToken { buffer },
            Self::TxToken {
                permit,
                buffer_pool: &mut self.buffer_pool,
            },
        ))
    }

    fn transmit(&mut self, _timestamp: Instant) -> Option<Self::TxToken<'_>> {
        // #region agent log
        // trace!("[SmlDev] TX out_capacity: {}", self.out_buf.capacity());
        match self.out_buf.try_reserve() {
            Ok(permit) => Some(Self::TxToken {
                permit,
                buffer_pool: &mut self.buffer_pool,
            }),
            Err(_) => {
                // debug_log(
                //     "device.rs:transmit",
                //     "transmit None",
                //     "B",
                //     "egress_full_try_reserve",
                // );
                None
            }
        }
        // #endregion
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut capabilities = DeviceCapabilities::default();
        capabilities.medium = Medium::Ip;
        capabilities.max_transmission_unit = self.max_transmission_unit;
        capabilities
    }
}

pub(super) struct VirtualRxToken {
    buffer: Bytes,
}

impl RxToken for VirtualRxToken {
    fn consume<R, F>(self, f: F) -> R
    where
        F: FnOnce(&[u8]) -> R,
    {
        f(&self.buffer[..])
    }
}

pub(super) struct VirtualTxToken<'a> {
    permit: Permit<'a, Bytes>,
    buffer_pool: &'a mut BufferPool,
}

impl<'a> TxToken for VirtualTxToken<'a> {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let mut buffer = self.buffer_pool.get_dirty_buffer(len);
        let result = f(&mut buffer);
        self.permit.send(buffer.freeze());
        result
    }
}
