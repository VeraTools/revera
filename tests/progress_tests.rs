use revera::progress::{ProgressBroadcaster, ProgressEvent};

#[test]
fn progress_event_json_serialization() {
    let ev = ProgressEvent::DiffParsed {
        files_changed: 4,
        bytes: 1024,
    };
    let json_str = ev.to_json_line();
    assert!(json_str.contains(r#""event":"diff_parsed""#));
    assert!(json_str.contains(r#""files_changed":4"#));
    assert!(json_str.contains(r#""bytes":1024"#));

    let ev2 = ProgressEvent::StaticRulesChecked { matches_found: 3 };
    let json_str2 = ev2.to_json_line();
    assert!(json_str2.contains(r#""event":"static_rules_checked""#));
    assert!(json_str2.contains(r#""matches_found":3"#));

    let ev3 = ProgressEvent::CandidatesAggregated {
        raw: 10,
        unique: 6,
        consensus: 2,
    };
    let json_str3 = ev3.to_json_line();
    assert!(json_str3.contains(r#""event":"candidates_aggregated""#));
    assert!(json_str3.contains(r#""consensus":2"#));
}

#[tokio::test]
async fn broadcaster_subscriber_receives_events() {
    let broadcaster = ProgressBroadcaster::new(16);
    let mut rx1 = broadcaster.subscribe();
    let mut rx2 = broadcaster.subscribe();

    broadcaster.emit(ProgressEvent::ScoutDispatched {
        lane: "security".into(),
        model: "claude-haiku".into(),
    });
    broadcaster.emit(ProgressEvent::ValidationStarted { count: 3 });

    let msg1_rx1 = rx1.recv().await.unwrap();
    let msg1_rx2 = rx2.recv().await.unwrap();
    assert_eq!(
        msg1_rx1,
        ProgressEvent::ScoutDispatched {
            lane: "security".into(),
            model: "claude-haiku".into(),
        }
    );
    assert_eq!(msg1_rx1, msg1_rx2);

    let msg2_rx1 = rx1.recv().await.unwrap();
    assert_eq!(msg2_rx1, ProgressEvent::ValidationStarted { count: 3 });
}
