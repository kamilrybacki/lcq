//! The frame the hub hands a receiver survives the wire unchanged.

use lcq::infrastructure::hub::{
    DELIVERY_HEADER_BYTES, Delivery, Inbound, PREAMBLE_BYTES, Preamble, Verdict,
};

#[test]
fn a_delivery_round_trips() {
    let delivery = Delivery {
        bytes: vec![1, 2, 3, 250],
        rssi_dbm: -131,
        snr_db: -14,
        airtime_ms: 1313,
        verdict: Verdict::Decoded,
    };
    let encoded = delivery.encode();
    assert_eq!(encoded.len(), DELIVERY_HEADER_BYTES + 4);
    assert_eq!(Delivery::decode(&encoded), Some(delivery));
}

#[test]
fn a_wreck_keeps_its_flag() {
    let delivery = Delivery {
        bytes: vec![0xA5; 17],
        rssi_dbm: -80,
        snr_db: 12,
        airtime_ms: 5,
        verdict: Verdict::CrcError,
    };
    assert_eq!(Delivery::decode(&delivery.encode()), Some(delivery));
}

#[test]
fn garbage_is_not_a_delivery() {
    assert_eq!(Delivery::decode(&[]), None);
    assert_eq!(Delivery::decode(&[0x7F; 12]), None, "unknown tag");
    let mut bad_flag = Delivery {
        bytes: vec![],
        rssi_dbm: 0,
        snr_db: 0,
        airtime_ms: 0,
        verdict: Verdict::HeaderError,
    }
    .encode();
    bad_flag[8] = 7;
    assert_eq!(Delivery::decode(&bad_flag), None);
}

#[test]
fn a_preamble_notice_round_trips_and_is_told_apart_from_a_delivery() {
    let preamble = Preamble {
        rssi_dbm: -118,
        airtime_ms: 1394,
    };
    let encoded = preamble.encode();
    assert_eq!(encoded.len(), PREAMBLE_BYTES);
    assert_eq!(Inbound::decode(&encoded), Some(Inbound::Preamble(preamble)));
    let delivery = Delivery {
        bytes: vec![9],
        rssi_dbm: -80,
        snr_db: 3,
        airtime_ms: 5,
        verdict: Verdict::Decoded,
    };
    assert_eq!(
        Inbound::decode(&delivery.encode()),
        Some(Inbound::Delivery(delivery))
    );
    assert_eq!(Inbound::decode(&[0x03; 3]), None, "a truncated notice");
}
