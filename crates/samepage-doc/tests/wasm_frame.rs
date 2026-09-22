//! A frame produced by the real doc.wasm replica is admitted by the native
//! host. Run through `web/run-check.sh`, which writes the frame first.

mod support;

use samepage_capability::Scalar;
use samepage_doc::host::FrameOutcome;
use support::*;

#[test]
#[ignore = "needs a frame from the doc.wasm check; run web/run-check.sh"]
fn a_frame_from_the_wasm_replica_commits_with_the_host_stamp() {
    let path = std::env::var("SAMEPAGE_DOC_WASM_FRAME").expect("set SAMEPAGE_DOC_WASM_FRAME");
    let frame = std::fs::read(path).unwrap();
    let host = host();
    let reply = host.receive_frame(&alice(), &frame).unwrap();
    assert!(
        matches!(reply.outcome, FrameOutcome::Committed { .. }),
        "{:?}",
        reply.outcome
    );
    let view = host.view();
    let card = view
        .records()
        .find(|r| r.kind == "card")
        .expect("the wasm replica's card");
    assert_eq!(card.props["label"], Scalar::from("From wasm"));
    assert_eq!(
        card.props["created_by_participant"],
        Scalar::from("p-alice")
    );
    println!(
        "WASM FRAME admitted: {} bytes, card {}",
        frame.len(),
        card.id
    );
}
