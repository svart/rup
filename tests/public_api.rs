use std::time::Duration;

use rup::{PacketSize, PingConfig, PingReport, Pinger, Protocol, TrafficClass};

#[test]
fn high_level_builders_compose_without_exposing_internal_state() {
    let remote = "127.0.0.1:5000".parse().unwrap();
    let packet_size = PacketSize::new(64).unwrap();

    let _config = PingConfig::new(remote, Protocol::Udp)
        .count(5)
        .interval(Duration::from_millis(250))
        .timeout(Duration::from_secs(2))
        .request_size(packet_size)
        .response_size(packet_size)
        .traffic_class(TrafficClass::new(184));

    let _pinger = Pinger::new(remote, Protocol::Udp)
        .count(5)
        .interval(Duration::from_millis(250))
        .timeout(Duration::from_secs(2))
        .request_size(packet_size)
        .response_size(packet_size)
        .traffic_class(TrafficClass::new(184));
}

#[test]
fn report_accessors_preserve_received_sample_consistency() {
    let report =
        PingReport::from_rtts(3, vec![Duration::from_millis(2), Duration::from_millis(4)]).unwrap();

    assert_eq!(report.sent(), 3);
    assert_eq!(report.received(), 2);
    assert_eq!(report.rtts().len(), report.received() as usize);
    assert_eq!(report.mean(), Some(Duration::from_millis(3)));
}

#[test]
fn report_rejects_more_samples_than_sent_packets() {
    let error = PingReport::from_rtts(0, vec![Duration::from_millis(1)]).unwrap_err();

    assert_eq!(error.to_string(), "received count 1 exceeds sent count 0");
}
