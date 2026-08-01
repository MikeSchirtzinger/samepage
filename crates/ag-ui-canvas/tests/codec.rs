//! Frame codec tests — the alignment invariant is the whole point of the
//! binary channel, so it gets asserted from several directions.

use ag_ui_canvas::blob::AlignedBytes;
use ag_ui_canvas::codec::{
    decode_frame, encode_blob, encode_blob_ack, encode_blob_request, encode_sync, BlobHeader,
    CodecError, Frame, BLOB_ACK_LEN, BLOB_HEADER_LEN, BLOB_REQUEST_LEN, FRAME_MAGIC,
};
use ag_ui_canvas::scene::{authorized_blob_color, BlobRef, ObjectSnapshot};
use ag_ui_canvas::DType;
use ag_ui_canvas::ObjectId;

fn f32_payload(n: usize) -> Vec<f32> {
    (0..n).map(|i| i as f32 * 0.5 - 100.0).collect()
}

fn header(n: usize) -> BlobHeader {
    BlobHeader {
        dtype: DType::F32,
        ndim: 2,
        blob_id: 0xDEAD_BEEF_CAFE_F00D,
        generation: 7,
        element_count: n as u32,
        shape: [n as u32 / 2, 2],
    }
}

#[test]
fn header_len_is_multiple_of_8() {
    assert_eq!(BLOB_HEADER_LEN % 8, 0);
}

#[test]
fn blob_roundtrip() {
    let floats = f32_payload(64);
    let payload: &[u8] = bytemuck::cast_slice(&floats);
    let h = header(64);

    let encoded = encode_blob(&h, payload).expect("valid blob should encode");
    match decode_frame(encoded.as_bytes()).expect("decode") {
        Frame::Blob {
            header: got,
            payload: got_payload,
        } => {
            assert_eq!(got, h);
            assert_eq!(got_payload, payload);
            // In-place reinterpretation must succeed (bytemuck checks
            // alignment at runtime and panics on violation).
            let as_floats: &[f32] = bytemuck::cast_slice(got_payload);
            assert_eq!(as_floats, &floats[..]);
        }
        other => panic!("expected Blob, got {other:?}"),
    }
}

#[test]
fn blob_payload_is_8_aligned_in_aligned_receive_buffer() {
    // Simulate the receive path: wire bytes land in an AlignedBytes buffer.
    let floats = f32_payload(100_000 * 2); // 100k points x 2 dims — past u16 range
    let h = BlobHeader {
        element_count: floats.len() as u32,
        shape: [100_000, 2],
        ..header(0)
    };
    let wire = encode_blob(&h, bytemuck::cast_slice(&floats)).expect("valid blob should encode");
    let received = AlignedBytes::from_bytes(wire.as_bytes());

    let Frame::Blob { payload, .. } = decode_frame(received.as_bytes()).unwrap() else {
        panic!("expected Blob");
    };
    assert_eq!(
        payload.as_ptr() as usize % 8,
        0,
        "payload base must be 8-aligned"
    );
    assert_eq!(payload.len(), floats.len() * 4);
}

#[test]
fn blob_in_unaligned_buffer_is_rejected_not_ub() {
    // Receive into a deliberately misaligned buffer (offset 1 from an aligned
    // base). decode_frame must refuse rather than letting a cast blow up.
    let floats = f32_payload(8);
    let wire =
        encode_blob(&header(8), bytemuck::cast_slice(&floats)).expect("valid blob should encode");

    let mut shifted = AlignedBytes::zeroed(wire.len() + 1);
    shifted.as_bytes_mut()[1..].copy_from_slice(wire.as_bytes());
    match decode_frame(&shifted.as_bytes()[1..]) {
        Err(CodecError::Misaligned { needs: 4 }) => {}
        other => panic!("expected Misaligned error, got {other:?}"),
    }
}

#[test]
fn payload_length_mismatch_is_rejected() {
    let floats = f32_payload(8);
    let wire =
        encode_blob(&header(8), bytemuck::cast_slice(&floats)).expect("valid blob should encode");

    // Corrupt the element count in the encoded header: 9 elements claimed,
    // 8 elements (32 bytes) present.
    let mut corrupted = AlignedBytes::from_bytes(wire.as_bytes());
    corrupted.as_bytes_mut()[20..24].copy_from_slice(&9u32.to_le_bytes());
    match decode_frame(corrupted.as_bytes()) {
        Err(CodecError::PayloadLen { got: 32, want: 36 }) => {}
        other => panic!("expected PayloadLen error, got {other:?}"),
    }
}

#[test]
fn sync_frame_roundtrip() {
    let payload = b"\x00\x00\x01\x02\x03"; // opaque y-sync bytes
    let wire = encode_sync(payload);
    match decode_frame(&wire).unwrap() {
        Frame::Sync(got) => assert_eq!(got, payload),
        other => panic!("expected Sync, got {other:?}"),
    }
}

#[test]
fn request_and_ack_roundtrip() {
    let mut wire = encode_blob_request(42, 3);
    assert_eq!(
        decode_frame(&wire).unwrap(),
        Frame::BlobRequest {
            blob_id: 42,
            generation: 3
        }
    );
    wire.push(0);
    assert!(matches!(
        decode_frame(&wire),
        Err(CodecError::FrameLen {
            got,
            want: BLOB_REQUEST_LEN
        }) if got == BLOB_REQUEST_LEN + 1
    ));

    let mut wire = encode_blob_ack(42, 3, 1);
    assert_eq!(
        decode_frame(&wire).unwrap(),
        Frame::BlobAck {
            blob_id: 42,
            generation: 3,
            status: 1
        }
    );
    wire.push(0);
    assert!(matches!(
        decode_frame(&wire),
        Err(CodecError::FrameLen {
            got,
            want: BLOB_ACK_LEN
        }) if got == BLOB_ACK_LEN + 1
    ));
}

#[test]
fn garbage_is_rejected_not_panicked() {
    assert!(matches!(decode_frame(&[]), Err(CodecError::Truncated(0))));
    assert!(matches!(
        decode_frame(&[0x00, 0x01, 0x01, 0x00]),
        Err(CodecError::BadMagic(0x00))
    ));
    assert!(matches!(
        decode_frame(&[FRAME_MAGIC, 0x99, 0x01, 0x00]),
        Err(CodecError::BadVersion(0x99))
    ));
    assert!(matches!(
        decode_frame(&[FRAME_MAGIC, 0x01, 0x77, 0x00]),
        Err(CodecError::BadFrameType(0x77))
    ));
    assert!(matches!(
        decode_frame(&[FRAME_MAGIC, 0x01, 0x01, 0x01]),
        Err(CodecError::UnsupportedFlags(0x01))
    ));
    // Truncated blob header
    let mut short = vec![FRAME_MAGIC, 0x01, 0x02, 0x00];
    short.extend_from_slice(&[1; 10]);
    assert!(matches!(
        decode_frame(&short),
        Err(CodecError::Truncated(14))
    ));
    // Unknown dtype
    let mut bad_dtype = AlignedBytes::zeroed(BLOB_HEADER_LEN);
    {
        let b = bad_dtype.as_bytes_mut();
        b[0] = FRAME_MAGIC;
        b[1] = 0x01;
        b[2] = 0x02;
        b[4] = 0xEE;
    }
    assert!(matches!(
        decode_frame(bad_dtype.as_bytes()),
        Err(CodecError::BadDType(0xEE))
    ));
}

#[test]
fn version_one_reserved_bytes_must_be_zero() {
    let payload = f32_payload(4);
    let bytes: &[u8] = bytemuck::cast_slice(&payload);
    let mut blob = encode_blob(&header(4), bytes).expect("valid blob");
    blob.as_bytes_mut()[6] = 1;
    assert!(matches!(
        decode_frame(blob.as_bytes()),
        Err(CodecError::NonZeroReserved {
            offset: 6,
            value: 1
        })
    ));

    let mut blob = encode_blob(&header(4), bytes).expect("valid blob");
    blob.as_bytes_mut()[32] = 2;
    assert!(matches!(
        decode_frame(blob.as_bytes()),
        Err(CodecError::NonZeroReserved {
            offset: 32,
            value: 2
        })
    ));

    let mut request = encode_blob_request(42, 3);
    request[4] = 3;
    assert!(matches!(
        decode_frame(&request),
        Err(CodecError::NonZeroReserved {
            offset: 4,
            value: 3
        })
    ));

    let mut ack = encode_blob_ack(42, 3, 1);
    ack[7] = 4;
    assert!(matches!(
        decode_frame(&ack),
        Err(CodecError::NonZeroReserved {
            offset: 7,
            value: 4
        })
    ));
}

#[test]
fn blob_reference_authorizes_only_matching_vec2_f32_stream_frames() {
    let reference = BlobRef {
        blob_id: 7,
        generation: 3,
        count: 8,
        dtype: DType::F32,
    };
    let mut frame = BlobHeader {
        dtype: DType::F32,
        ndim: 2,
        blob_id: 7,
        generation: 3,
        element_count: 8,
        shape: [4, 2],
    };
    assert!(reference.accepts_header(&frame));
    frame.generation = 4;
    assert!(
        reference.accepts_header(&frame),
        "new stream frames are valid"
    );
    frame.generation = 2;
    assert!(!reference.accepts_header(&frame), "stale epoch is rejected");
    frame.generation = 3;
    frame.blob_id = 8;
    assert!(!reference.accepts_header(&frame));
    frame.blob_id = 7;
    frame.shape = [8, 1];
    assert!(
        !reference.accepts_header(&frame),
        "non-vec2 shape is rejected"
    );
    frame.shape = [4, 2];
    frame.dtype = DType::U32;
    assert!(
        !reference.accepts_header(&frame),
        "non-f32 payload is rejected"
    );
}

#[test]
fn only_a_live_matching_particle_object_authorizes_blob_color() {
    let reference = BlobRef {
        blob_id: 7,
        generation: 3,
        count: 8,
        dtype: DType::F32,
    };
    let object = ObjectSnapshot {
        id: ObjectId::from("cloud"),
        kind: "particles".to_string(),
        owner: "agent".to_string(),
        x: 0.0,
        y: 0.0,
        x2: 0.0,
        y2: 0.0,
        scale: 1.0,
        color: 0x123456FF,
        z_index: 0.0,
        blob_ref: Some(reference),
        text: None,
    };
    let mut frame = BlobHeader {
        dtype: DType::F32,
        ndim: 2,
        blob_id: 7,
        generation: 4,
        element_count: 8,
        shape: [4, 2],
    };
    assert_eq!(
        authorized_blob_color(std::slice::from_ref(&object), &frame),
        Some(0x123456FF)
    );
    assert_eq!(
        authorized_blob_color(&[], &frame),
        None,
        "clear rejects delayed blob"
    );

    let mut wrong_kind = object.clone();
    wrong_kind.kind = "disc".to_string();
    assert_eq!(authorized_blob_color(&[wrong_kind], &frame), None);
    frame.element_count = 6;
    frame.shape = [3, 2];
    assert_eq!(
        authorized_blob_color(std::slice::from_ref(&object), &frame),
        None
    );
    frame.element_count = 8;
    frame.shape = [4, 2];
    frame.generation = 2;
    assert_eq!(authorized_blob_color(&[object], &frame), None);
}
