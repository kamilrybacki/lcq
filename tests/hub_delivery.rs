//! The frame the hub hands a receiver survives the wire unchanged.

use lcq::infrastructure::hub::{DELIVERY_HEADER_BYTES, Delivery};

#[test]
fn a_delivery_round_trips() {
    let delivery = Delivery {
        bytes: vec![1, 2, 3, 250],
        rssi_dbm: -131,
        snr_db: -14,
        airtime_ms: 1313,
        crc_ok: true,
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
        crc_ok: false,
    };
    assert_eq!(Delivery::decode(&delivery.encode()), Some(delivery));
}

#[test]
fn garbage_is_not_a_delivery() {
    assert_eq!(Delivery::decode(&[]), None);
    assert_eq!(Delivery::decode(&[0x02; 12]), None, "unknown tag");
    let mut bad_flag = Delivery {
        bytes: vec![],
        rssi_dbm: 0,
        snr_db: 0,
        airtime_ms: 0,
        crc_ok: true,
    }
    .encode();
    bad_flag[8] = 7;
    assert_eq!(Delivery::decode(&bad_flag), None);
}
