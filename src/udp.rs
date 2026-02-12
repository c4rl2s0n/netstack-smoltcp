/// Changelog:
/// - use Bytes instead of Vec<u8>
use std::{
    net::SocketAddr,
    pin::Pin,
    task::{Context, Poll},
};

use etherparse::PacketBuilder;
use futures::{Sink, SinkExt, Stream, ready};
use smoltcp::wire::UdpPacket;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio_util::{
    bytes::{BufMut, Bytes},
    sync::PollSender,
};
use tracing::{error, trace};

use crate::{
    BufferPool,
    packet::{AnyIpPktFrame, IpPacket},
};

pub type UdpMsg = (
    Bytes,      /* payload */
    SocketAddr, /* local */
    SocketAddr, /* remote */
);

const WRITE_POOL_SIZE: usize = 32 * 1024;

pub struct UdpSocket {
    udp_rx: Receiver<AnyIpPktFrame>,
    stack_tx: PollSender<AnyIpPktFrame>,
}

impl UdpSocket {
    pub(super) fn new(udp_rx: Receiver<AnyIpPktFrame>, stack_tx: Sender<AnyIpPktFrame>) -> Self {
        Self {
            udp_rx,
            stack_tx: PollSender::new(stack_tx),
        }
    }

    pub fn split(self) -> (ReadHalf, WriteHalf) {
        (
            ReadHalf {
                udp_rx: self.udp_rx,
            },
            WriteHalf {
                stack_tx: self.stack_tx,
                buffer_pool: BufferPool::new(WRITE_POOL_SIZE),
            },
        )
    }
}

pub struct ReadHalf {
    udp_rx: Receiver<AnyIpPktFrame>,
}

pub struct WriteHalf {
    stack_tx: PollSender<AnyIpPktFrame>,
    buffer_pool: BufferPool,
}

impl Stream for ReadHalf {
    type Item = UdpMsg;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<Self::Item>> {
        self.udp_rx.poll_recv(cx).map(|item| {
            item.and_then(|frame| {
                let packet = match IpPacket::new_checked(&frame) {
                    Ok(p) => p,
                    Err(err) => {
                        error!("invalid IP packet: {}", err);
                        return None;
                    }
                };

                let src_ip = packet.src_addr();
                let dst_ip = packet.dst_addr();
                let payload = packet.payload();
                let mut payload_offset = packet.header_len();

                let packet: UdpPacket<&[u8]> = match UdpPacket::new_checked(payload) {
                    Ok(p) => p,
                    Err(err) => {
                        error!("invalid err: {err}, src_ip: {src_ip}, dst_ip: {dst_ip}, payload: {payload:?}");
                        return None;
                    }
                };
                let src_port = packet.src_port();
                let dst_port = packet.dst_port();

                let src_addr = SocketAddr::new(src_ip, src_port);
                let dst_addr = SocketAddr::new(dst_ip, dst_port);
                payload_offset += 8;

                trace!("created UDP socket for {} <-> {}", src_addr, dst_addr);

                let payload = frame.slice(payload_offset..payload_offset + packet.payload().len());
                Some((payload, src_addr, dst_addr))
            })
        })
    }
}

impl Sink<UdpMsg> for WriteHalf {
    type Error = std::io::Error;

    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        match ready!(self.stack_tx.poll_ready_unpin(cx)) {
            Ok(()) => Poll::Ready(Ok(())),
            Err(err) => Poll::Ready(Err(std::io::Error::other(err))),
        }
    }

    fn start_send(self: Pin<&mut Self>, item: UdpMsg) -> Result<(), Self::Error> {
        use std::io::{Error, ErrorKind::InvalidData};
        let (data, src_addr, dst_addr) = item;

        if data.is_empty() {
            return Ok(());
        }

        let builder = match (src_addr, dst_addr) {
            (SocketAddr::V4(src), SocketAddr::V4(dst)) => {
                PacketBuilder::ipv4(src.ip().octets(), dst.ip().octets(), 20)
                    .udp(src_addr.port(), dst_addr.port())
            }
            (SocketAddr::V6(src), SocketAddr::V6(dst)) => {
                PacketBuilder::ipv6(src.ip().octets(), dst.ip().octets(), 20)
                    .udp(src_addr.port(), dst_addr.port())
            }
            _ => {
                return Err(Error::new(InvalidData, "src or destination type unmatch"));
            }
        };

        let this = self.get_mut();
        // get the required size for the packet
        let total_size = builder.size(data.len());
        // Get a dirty buffer that can hold the required capacity
        let mut packet_buffer = this.buffer_pool.get_dirty_buffer(total_size);
        let mut ip_packet_writer = (&mut packet_buffer).writer();

        // write the packet to the buffer
        builder
            .write(&mut ip_packet_writer, &data)
            .map_err(|err| Error::other(format!("PacketBuilder::write: {err}")))?;

        //
        let packet_to_send = packet_buffer.freeze();
        match this.stack_tx.start_send_unpin(packet_to_send) {
            Ok(()) => Ok(()),
            Err(err) => Err(Error::other(format!("send error: {err}"))),
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        use std::io::Error;
        match ready!(self.stack_tx.poll_flush_unpin(cx)) {
            Ok(()) => Poll::Ready(Ok(())),
            Err(err) => Poll::Ready(Err(Error::other(format!("flush error: {err}")))),
        }
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        use std::io::Error;
        match ready!(self.stack_tx.poll_close_unpin(cx)) {
            Ok(()) => Poll::Ready(Ok(())),
            Err(err) => Poll::Ready(Err(Error::other(format!("close error: {err}")))),
        }
    }
}
