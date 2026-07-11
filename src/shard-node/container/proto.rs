// src/proto.rs
//
// Wire protocol between server (machine B) and client (machine A).
//
// All messages are fixed-size 3-byte frames so we never need heap allocation
// or a length-prefixed body for control traffic.
//
//  [ TAG: 1 byte ][ STREAM_ID: 2 bytes, big-endian ]
//
// Tags:
//   0x01  PING         Server → Client  keepalive request
//   0x02  PONG         Client → Server  keepalive reply
//   0x03  OPEN         Server → Client  "a public visitor just connected, open a data channel for stream id X"
//   0x04  CLOSE        Either direction  "stream X is done"

pub const TAG_PING:  u8 = 0x01;
pub const TAG_PONG:  u8 = 0x02;
pub const TAG_OPEN:  u8 = 0x03;
pub const TAG_CLOSE: u8 = 0x04;
pub const TAG_DATA_PORT: u8 = 0x05;

pub const FRAME_LEN: usize = 3;

pub fn encode(tag: u8, stream_id: u16) -> [u8; FRAME_LEN] {
    let [hi, lo] = stream_id.to_be_bytes();
    [tag, hi, lo]
}

pub fn decode(buf: &[u8; FRAME_LEN]) -> (u8, u16) {
    let tag = buf[0];
    let stream_id = u16::from_be_bytes([buf[1], buf[2]]);
    (tag, stream_id)
}
