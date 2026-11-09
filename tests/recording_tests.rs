//! Integration tests for the public MP4 muxer (`rafdev_playlink::recording`).

use rafdev_playlink::recording::mux_h264_to_mp4;

/// Three synthetic access units: SPS + PPS + IDR, then two non-IDR slices.
/// The NAL payloads are header-plausible but not decodable; the muxer only
/// reads NAL headers and SPS/PPS bytes.
fn synthetic_packets() -> Vec<(u64, Vec<u8>)> {
    let sps: &[u8] = &[
        0x67, 0x64, 0x00, 0x1f, 0xac, 0xd9, 0x40, 0x78, 0x02, 0x27, 0xe5, 0x84,
    ];
    let pps: &[u8] = &[0x68, 0xee, 0x3c, 0x80];
    let idr: &[u8] = &[
        0x65, 0x88, 0x84, 0x00, 0x10, 0x00, 0x00, 0x03, 0x00, 0x80, 0x00, 0x00, 0x03, 0x00, 0x00,
        0x0f, 0x16, 0x2e, 0x48,
    ];
    let slice: &[u8] = &[0x41, 0x9a, 0x24, 0x00, 0x10, 0x00, 0x00, 0x03, 0x00, 0x80];
    let au = |nals: &[&[u8]]| -> Vec<u8> {
        let mut out = Vec::new();
        for nal in nals {
            out.extend_from_slice(&[0, 0, 0, 1]);
            out.extend_from_slice(nal);
        }
        out
    };
    vec![
        (0, au(&[sps, pps, idr])),
        (33_333, au(&[slice])),
        (66_666, au(&[slice])),
    ]
}

/// Walk the top-level boxes, returning (type, size) pairs. Asserts the
/// sizes are in range and cover the file exactly.
fn top_level_boxes(data: &[u8]) -> Vec<([u8; 4], usize)> {
    let mut out = Vec::new();
    let mut off = 0;
    while off + 8 <= data.len() {
        let size = u32::from_be_bytes(data[off..off + 4].try_into().unwrap()) as usize;
        assert!(
            size >= 8 && off + size <= data.len(),
            "box size out of range"
        );
        let tag: [u8; 4] = data[off + 4..off + 8].try_into().unwrap();
        out.push((tag, size));
        off += size;
    }
    assert_eq!(off, data.len(), "no trailing bytes after the last box");
    out
}

#[test]
fn muxed_mp4_has_ftyp_moov_mdat_and_consistent_sizes() {
    let mp4 = mux_h264_to_mp4(&synthetic_packets(), 1080, 1920).expect("muxing succeeds");
    assert_eq!(&mp4[4..8], b"ftyp", "output starts with an ftyp box");
    let boxes = top_level_boxes(&mp4);
    assert_eq!(&boxes[0].0, b"ftyp", "first box is ftyp");
    assert!(boxes.iter().any(|(t, _)| t == b"moov"), "moov present");
    assert!(boxes.iter().any(|(t, _)| t == b"mdat"), "mdat present");
    let total: usize = boxes.iter().map(|(_, s)| s).sum();
    assert_eq!(total, mp4.len(), "box sizes add up to the file size");
    assert!(
        mp4.windows(4).any(|w| w == b"avcC"),
        "moov contains an avcC box"
    );
}

#[test]
fn mux_fails_without_sps_pps_so_caller_can_fall_back_to_raw_h264() {
    let packets = vec![(0u64, vec![0, 0, 0, 1, 0x65, 0x88, 0x84])];
    assert!(mux_h264_to_mp4(&packets, 1080, 1920).is_err());
}
