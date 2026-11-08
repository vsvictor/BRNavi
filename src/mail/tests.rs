use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

fn notice() -> PasswordResetNotice {
    PasswordResetNotice {
        email: "user@example.com".into(),
        reset_token: "token-123".into(),
        reset_code: "123456".into(),
        user_id: "u1".into(),
    }
}

#[test]
fn message_is_addressed_and_contains_code() {
    let from: Mailbox = "XRNavi <admin@example.com>".parse().unwrap();
    let message = reset_message(&from, &notice()).unwrap();
    let raw = String::from_utf8(message.formatted()).unwrap().replace("=\r\n", "");
    assert!(raw.contains("user@example.com"));
    assert!(raw.contains("admin@example.com"));
    assert!(raw.contains("123456"));
    assert!(!raw.contains("token-123"));
}

#[test]
fn invalid_recipient_is_rejected() {
    let from: Mailbox = "admin@example.com".parse().unwrap();
    let mut bad = notice();
    bad.email = "not an address".into();
    assert!(reset_message(&from, &bad).is_err());
}

#[test]
fn combine_runs_every_notifier() {
    let count = Arc::new(AtomicUsize::new(0));
    let make = |count: Arc<AtomicUsize>| -> PasswordResetNotifier {
        Arc::new(move |_| {
            count.fetch_add(1, Ordering::SeqCst);
        })
    };
    combine(vec![make(count.clone()), make(count.clone())])(notice());
    assert_eq!(count.load(Ordering::SeqCst), 2);
}
