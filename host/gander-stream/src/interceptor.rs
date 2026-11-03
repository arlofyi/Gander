// SPDX-License-Identifier: Apache-2.0
//! Delivers inbound PLI and FIR to the application (condition (b) of the D12 acceptance).
//!
//! rtc 0.21 ends every inbound RTCP packet at the interceptor chain's terminus unless an
//! interceptor attaches `Attribute::DeliverToApplication`. WP-43 measured 0 of 16 PLIs reaching
//! `TrackLocal::poll` without this interceptor and 21 of 21 with it. It sits application-most
//! (slot [`SLOT`]), after every interceptor that reads RTCP itself.

use std::collections::VecDeque;
use std::time::Instant;

use rtc::interceptor::{Attribute, Interceptor, Packet, StreamInfo, TaggedPacket};
use rtc::rtcp::payload_feedbacks::full_intra_request::FullIntraRequest;
use rtc::rtcp::payload_feedbacks::picture_loss_indication::PictureLossIndication;

/// Registry slot (application-most).
pub const SLOT: usize = 14_000;

/// True for a PLI or FIR packet.
pub fn is_keyframe_request(p: &dyn rtc::rtcp::Packet) -> bool {
    let a = p.as_any();
    a.is::<PictureLossIndication>() || a.is::<FullIntraRequest>()
}

/// The interceptor: marks PLI/FIR for delivery, passes everything else through unchanged.
#[derive(Default)]
pub struct KeyframeRequests {
    reads: VecDeque<TaggedPacket>,
    writes: VecDeque<TaggedPacket>,
}

impl rtc::sansio::Protocol<TaggedPacket, TaggedPacket, ()> for KeyframeRequests {
    type Rout = TaggedPacket;
    type Wout = TaggedPacket;
    type Eout = ();
    type Error = rtc::shared::error::Error;
    type Time = Instant;

    fn handle_read(&mut self, mut msg: TaggedPacket) -> Result<(), Self::Error> {
        if let Packet::Rtcp(pkts) = &msg.message.packet
            && pkts.iter().any(|p| is_keyframe_request(p.as_ref()))
        {
            msg.message.add(Attribute::DeliverToApplication);
        }
        self.reads.push_back(msg);
        Ok(())
    }
    fn poll_read(&mut self) -> Option<TaggedPacket> {
        self.reads.pop_front()
    }
    fn handle_write(&mut self, msg: TaggedPacket) -> Result<(), Self::Error> {
        self.writes.push_back(msg);
        Ok(())
    }
    fn poll_write(&mut self) -> Option<TaggedPacket> {
        self.writes.pop_front()
    }
}

impl std::fmt::Debug for KeyframeRequests {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeyframeRequests").finish_non_exhaustive()
    }
}

impl Interceptor for KeyframeRequests {
    fn bind_local_stream(&mut self, _: &StreamInfo) {}
    fn unbind_local_stream(&mut self, _: &StreamInfo) {}
    fn bind_remote_stream(&mut self, _: &StreamInfo) {}
    fn unbind_remote_stream(&mut self, _: &StreamInfo) {}
}
