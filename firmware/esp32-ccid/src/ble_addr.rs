//! Static-random BLE address derivation (pure, always compiled —
//! host-testable; the ble builds consume it from `ble_debug`).

/// Force the static-device-address top bits (MSB |= 0b11) onto a
/// factory MAC so every unit advertises a DISTINCT address — per-arch
/// constants collided between same-architecture boards (Codex P2 on
/// #231). Bluetooth Core Vol 6, Part B, 1.3.1: "the two most
/// significant bits of [a static device] address shall be equal to 1".
pub fn static_random_from_mac(mac: [u8; 6]) -> [u8; 6] {
    let mut addr = mac;
    addr[0] |= 0xC0;
    addr
}

#[cfg(test)]
mod tests {
    use super::static_random_from_mac;

    #[test]
    fn forces_static_random_top_bits_msb() {
        assert_eq!(static_random_from_mac([0x4C, 1, 2, 3, 4, 0x9E])[0], 0xCC);
        assert_eq!(static_random_from_mac([0x90, 1, 2, 3, 4, 5])[0], 0xD0);
        // already-static input stays stable (idempotent)
        assert_eq!(
            static_random_from_mac([0xC4, 0x75, 0x25, 0xCB, 0x92, 0x9E]),
            [0xC4, 0x75, 0x25, 0xCB, 0x92, 0x9E]
        );
    }

    #[test]
    fn distinct_inputs_stay_distinct() {
        let a = static_random_from_mac([0x4C, 0x75, 0x25, 0xCB, 0x92, 0x9E]);
        let b = static_random_from_mac([0x4C, 0x75, 0x25, 0xCB, 0x92, 0x9D]);
        assert_ne!(
            a, b,
            "two same-arch units must not share an on-air identity"
        );
        assert_eq!(&a[1..], &[0x75, 0x25, 0xCB, 0x92, 0x9E], "tail preserved");
    }
}
