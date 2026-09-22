//! The y-sync framing: valid peers in any encoding order are accepted;
//! anything that is not whole messages is refused.

use samepage_doc::sync::{self, Incoming, SyncError};
use yrs::encoding::write::Write;
use yrs::updates::encoder::{Encode, Encoder, EncoderV1};
use yrs::{Map, StateVector, Transact};

/// A multi-client state vector, encoded with entries in the given order, as a
/// peer whose hash map iterates differently would send it.
fn step1_frame(entries: &[(u64, u32)]) -> Vec<u8> {
    let mut sv = EncoderV1::new();
    sv.write_var(entries.len() as u32);
    for (client, clock) in entries {
        sv.write_var(*client);
        sv.write_var(*clock);
    }
    let payload = sv.to_vec();
    let mut frame = EncoderV1::new();
    frame.write_var(0u8); // sync
    frame.write_var(0u8); // step 1
    frame.write_buf(&payload);
    frame.to_vec()
}

#[test]
fn a_multi_client_state_vector_is_accepted_in_every_order() {
    let entries = [(11u64, 3u32), (22, 5), (33, 7), (44, 1)];
    let orders: Vec<Vec<(u64, u32)>> = vec![
        entries.to_vec(),
        entries.iter().rev().cloned().collect(),
        vec![entries[2], entries[0], entries[3], entries[1]],
        vec![entries[1], entries[3], entries[0], entries[2]],
    ];
    for order in orders {
        let frame = step1_frame(&order);
        let parsed = sync::parse(&frame).unwrap_or_else(|e| panic!("{order:?}: {e}"));
        let [Incoming::Step1(sv)] = &parsed[..] else {
            panic!("{parsed:?}")
        };
        for (client, clock) in &entries {
            assert_eq!(sv.get(&yrs::block::ClientID::new(*client)), *clock);
        }
    }
}

#[test]
fn a_frame_from_a_real_yrs_peer_parses() {
    let doc = yrs::Doc::new();
    let map = doc.get_or_insert_map("objects");
    map.insert(&mut doc.transact_mut(), "a", "b");
    let sv = yrs::ReadTxn::state_vector(&doc.transact());
    let update = yrs::ReadTxn::encode_state_as_update_v1(&doc.transact(), &StateVector::default());
    let mut packed = sync::step1(&sv);
    packed.extend(sync::update(update.clone()));
    packed.extend(sync::step2(update.clone()));
    let parsed = sync::parse(&packed).unwrap();
    assert_eq!(parsed.len(), 3);
    assert!(matches!(&parsed[1], Incoming::Update(bytes) if bytes == &update));
}

#[test]
fn a_truncated_tail_or_unknown_message_is_refused() {
    let whole = step1_frame(&[(1, 1), (2, 2)]);
    let mut packed = whole.clone();
    packed.extend_from_slice(&whole);
    assert_eq!(sync::parse(&packed).unwrap().len(), 2);
    for cut in 1..whole.len() {
        let truncated = &packed[..whole.len() + cut];
        assert!(
            matches!(sync::parse(truncated), Err(SyncError::Malformed(_))),
            "a tail of {cut} bytes must be refused"
        );
    }
    let mut unknown = EncoderV1::new();
    unknown.write_var(0u8);
    unknown.write_var(9u8);
    unknown.write_buf(b"x");
    assert!(matches!(
        sync::parse(&unknown.to_vec()),
        Err(SyncError::Malformed(_))
    ));
    assert!(matches!(
        sync::parse(&yrs::sync::Message::AwarenessQuery.encode_v1()),
        Err(SyncError::Unsupported("awareness"))
    ));
    assert!(matches!(sync::parse(&[]), Err(SyncError::Empty)));
}
