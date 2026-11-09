//! `parse_devices_output` cases (integration tests).
//!
//! These tests exercise the public parser for `adb devices -l` output:
//! the header line is skipped, each row yields (serial, state, model),
//! and rows with unknown states are ignored.

use rafdev_playlink::core::ConnectionState;
use rafdev_playlink::device::parse_devices_output;

#[test]
fn header_only_yields_no_rows() {
    assert!(parse_devices_output("List of devices attached\n").is_empty());
    assert!(parse_devices_output("List of devices attached\n\n").is_empty());
    assert!(parse_devices_output("").is_empty());
}

#[test]
fn one_device_row_parses_serial_state_and_model() {
    let output = "List of devices attached\n\
        RF8M32KABCD\tdevice product:panther model:Pixel_7 device:panther transport_id:2\n";
    let rows = parse_devices_output(output);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, "RF8M32KABCD");
    assert_eq!(rows[0].1, ConnectionState::Connected);
    assert_eq!(rows[0].2, "Pixel_7");
}

#[test]
fn unauthorized_device_row() {
    let output = "List of devices attached\nRF8M32KABCD\tunauthorized\n";
    let rows = parse_devices_output(output);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, "RF8M32KABCD");
    assert_eq!(rows[0].1, ConnectionState::Unauthorized);
    assert_eq!(rows[0].2, "");
}

#[test]
fn offline_device_row() {
    let output = "List of devices attached\nHT7A1B2C3D4E\toffline\n";
    let rows = parse_devices_output(output);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, "HT7A1B2C3D4E");
    assert_eq!(rows[0].1, ConnectionState::Offline);
    assert_eq!(rows[0].2, "");
}

#[test]
fn missing_model_is_empty_string() {
    let output = "List of devices attached\nXYZ000\tdevice\n";
    let rows = parse_devices_output(output);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, "XYZ000");
    assert_eq!(rows[0].1, ConnectionState::Connected);
    assert_eq!(rows[0].2, "");
}

#[test]
fn mixed_list_keeps_order_and_skips_unknown_states() {
    let output = "List of devices attached\n\
        emulator-5554\tdevice product:sdk_gphone64_x86_64 model:sdk_gphone64_x86_64 device:emulator64_x86_64_arm64\n\
        RF8M32KABCD\tunauthorized\n\
        HT7A1B2C3D4E\toffline\n\
        BAD999\tno permissions\n\
        192.168.1.5:5555\tdevice product:panther model:Pixel_7 device:panther\n";
    let rows = parse_devices_output(output);
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[0].0, "emulator-5554");
    assert_eq!(rows[0].1, ConnectionState::Connected);
    assert_eq!(rows[0].2, "sdk_gphone64_x86_64");
    assert_eq!(rows[1].0, "RF8M32KABCD");
    assert_eq!(rows[1].1, ConnectionState::Unauthorized);
    assert_eq!(rows[2].0, "HT7A1B2C3D4E");
    assert_eq!(rows[2].1, ConnectionState::Offline);
    assert_eq!(rows[3].0, "192.168.1.5:5555");
    assert_eq!(rows[3].1, ConnectionState::Connected);
    assert_eq!(rows[3].2, "Pixel_7");
}
