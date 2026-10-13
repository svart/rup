use std::cmp::Ordering;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::pinger::{Entry, Response, StatEntry};
use crate::{PingEvent, PingReport, PingResult};

pub(super) struct MeasurementTracker {
    requests: VecDeque<Entry>,
    pending_responses: VecDeque<Response>,
    rtts: Vec<Duration>,
    sent: u64,
}

impl MeasurementTracker {
    pub(super) fn new() -> Self {
        Self {
            requests: VecDeque::new(),
            pending_responses: VecDeque::new(),
            rtts: Vec::new(),
            sent: 0,
        }
    }

    pub(super) fn ingest(&mut self, entry: StatEntry) -> Vec<PingEvent> {
        match entry {
            StatEntry::Open(request) => {
                self.sent += 1;
                self.requests.push_back(request);

                let mut events = Vec::new();
                while self.pending_responses.front().is_some_and(|response| {
                    self.requests
                        .back()
                        .is_some_and(|request| response.id <= request.id)
                }) {
                    let response = self
                        .pending_responses
                        .pop_front()
                        .expect("front checked above");
                    events.extend(self.match_response(response));
                }
                events
            }
            StatEntry::Close(response) => {
                if self
                    .requests
                    .back()
                    .is_none_or(|request| response.id > request.id)
                {
                    self.pending_responses.push_back(response);
                    Vec::new()
                } else {
                    self.match_response(response)
                }
            }
        }
    }

    pub(super) fn expire(&mut self, now: Instant, wait_time: Duration) -> Vec<PingEvent> {
        let mut events = Vec::new();
        while self
            .requests
            .front()
            .is_some_and(|request| now.saturating_duration_since(request.ts) >= wait_time)
        {
            let request = self.requests.pop_front().expect("front checked above");
            events.push(PingEvent::Timeout { seq: request.id });
        }
        events
    }

    pub(super) fn next_timeout(&self, now: Instant, wait_time: Duration) -> Option<Duration> {
        self.requests
            .front()
            .map(|request| wait_time.saturating_sub(now.saturating_duration_since(request.ts)))
    }

    pub(super) fn is_settled(&self) -> bool {
        self.requests.is_empty()
    }

    pub(super) fn into_report(self) -> PingReport {
        PingReport::from_rtts(self.sent, self.rtts)
            .expect("the tracker records at most one RTT per sent request")
    }

    fn match_response(&mut self, response: Response) -> Vec<PingEvent> {
        let mut events = Vec::new();

        while let Some(request) = self.requests.pop_front() {
            match response.id.cmp(&request.id) {
                Ordering::Greater => {
                    events.push(PingEvent::ReorderOrLoss { seq: request.id });
                    continue;
                }
                Ordering::Equal => {
                    let result = PingResult {
                        seq: response.id,
                        rtt: response.timestamp.saturating_duration_since(request.ts),
                        received_at: response.timestamp,
                        size: response.size,
                        ttl: response.ttl,
                    };
                    self.rtts.push(result.rtt);
                    events.push(PingEvent::Reply(result));
                }
                Ordering::Less => self.requests.push_front(request),
            }
            break;
        }

        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sent(id: u64, timestamp: Instant) -> StatEntry {
        StatEntry::Open(Entry { id, ts: timestamp })
    }

    fn received(id: u64, timestamp: Instant) -> StatEntry {
        StatEntry::Close(Response {
            id,
            timestamp,
            size: 12,
            ttl: Some(64),
        })
    }

    #[test]
    fn matches_sent_and_received_measurements() {
        let started = Instant::now();
        let mut tracker = MeasurementTracker::new();

        assert!(tracker.ingest(sent(7, started)).is_empty());
        assert_eq!(
            tracker.ingest(received(7, started + Duration::from_millis(3))),
            vec![PingEvent::Reply(PingResult {
                seq: 7,
                rtt: Duration::from_millis(3),
                received_at: started + Duration::from_millis(3),
                size: 12,
                ttl: Some(64),
            })]
        );

        let report = tracker.into_report();
        assert_eq!(report.sent, 1);
        assert_eq!(report.received, 1);
    }

    #[test]
    fn buffers_a_response_that_arrives_before_send_accounting() {
        let started = Instant::now();
        let mut tracker = MeasurementTracker::new();

        assert!(
            tracker
                .ingest(received(0, started + Duration::from_millis(2)))
                .is_empty()
        );
        assert!(matches!(
            tracker.ingest(sent(0, started)).as_slice(),
            [PingEvent::Reply(PingResult { seq: 0, received_at, .. })]
                if *received_at == started + Duration::from_millis(2)
        ));
    }

    #[test]
    fn reports_missing_sequences_before_a_later_response() {
        let started = Instant::now();
        let mut tracker = MeasurementTracker::new();
        tracker.ingest(sent(0, started));
        tracker.ingest(sent(1, started));
        tracker.ingest(sent(2, started));

        assert_eq!(
            tracker.ingest(received(2, started + Duration::from_millis(1))),
            vec![
                PingEvent::ReorderOrLoss { seq: 0 },
                PingEvent::ReorderOrLoss { seq: 1 },
                PingEvent::Reply(PingResult {
                    seq: 2,
                    rtt: Duration::from_millis(1),
                    received_at: started + Duration::from_millis(1),
                    size: 12,
                    ttl: Some(64),
                }),
            ]
        );
    }

    #[test]
    fn expiration_uses_an_explicit_time() {
        let started = Instant::now();
        let mut tracker = MeasurementTracker::new();
        tracker.ingest(sent(0, started));
        tracker.ingest(sent(1, started + Duration::from_millis(5)));

        assert_eq!(
            tracker.next_timeout(started, Duration::from_millis(10)),
            Some(Duration::from_millis(10))
        );
        assert_eq!(
            tracker.expire(
                started + Duration::from_millis(10),
                Duration::from_millis(10)
            ),
            vec![PingEvent::Timeout { seq: 0 }]
        );
        assert!(!tracker.is_settled());
    }

    #[test]
    fn an_early_response_timestamp_saturates_instead_of_panicking() {
        let started = Instant::now();
        let mut tracker = MeasurementTracker::new();
        tracker.ingest(sent(0, started + Duration::from_millis(1)));

        assert!(matches!(
            tracker.ingest(received(0, started)).as_slice(),
            [PingEvent::Reply(PingResult {
                rtt: Duration::ZERO,
                ..
            })]
        ));
    }
}
