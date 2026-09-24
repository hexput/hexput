//! Story 3.2: the capability decision itself (AD-3) — a blanket grant lets a call go ahead, and
//! every refusal is the same error whatever its reason.

use hexput_enforce::{Capabilities, Reason};
use hexput_shared::diagnostics::Span;

fn span() -> Span {
    Span::new(4, 11, 1, 5)
}

fn session() -> Capabilities {
    Capabilities::registered([("getOrder", true), ("sendMail", false)])
}

#[test]
fn a_blanket_granted_function_may_be_called() {
    assert!(session().check_call("getOrder", span()).is_ok());
}

#[test]
fn a_registered_function_without_a_grant_is_refused_as_not_granted() {
    let refusal = session().check_call("sendMail", span()).unwrap_err();
    assert_eq!(refusal.reason(), Reason::NotGranted);
    assert_eq!(refusal.reason().as_str(), "not_granted");
    let error = refusal.diagnostic();
    assert_eq!(error.category.as_str(), "capability");
    assert_eq!(error.code.as_str(), "capability.unknown_function");
    assert_eq!(error.span, span());
}

#[test]
fn an_unregistered_function_is_refused_as_unregistered() {
    let refusal = session().check_call("nope", span()).unwrap_err();
    assert_eq!(refusal.reason(), Reason::Unregistered);
    assert_eq!(refusal.reason().as_str(), "unregistered");
}

#[test]
fn not_granted_and_unregistered_raise_the_same_error() {
    // One name, registered without a grant in one Session and not at all in another: the Script
    // sees exactly the same diagnostic.
    let withheld = Capabilities::registered([("getOrder", false)])
        .check_call("getOrder", span())
        .unwrap_err();
    let absent = Capabilities::registered([("other", true)])
        .check_call("getOrder", span())
        .unwrap_err();
    assert_ne!(withheld.reason(), absent.reason());
    assert_eq!(withheld.into_diagnostic(), absent.into_diagnostic());
}

#[test]
fn nothing_is_callable_with_no_capabilities() {
    let refusal = Capabilities::none()
        .check_call("getOrder", span())
        .unwrap_err();
    assert_eq!(refusal.reason(), Reason::Unregistered);
}
