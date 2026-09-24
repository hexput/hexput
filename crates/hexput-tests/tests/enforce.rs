//! Story 3.2: the capability decision itself (AD-3) — a blanket grant lets a call go ahead, and
//! every refusal is the same error whatever its reason. Story 3.3: a function registered without
//! a blanket grant is decided per call by the Backend's handler; only an explicit `true` allows it.

use hexput_enforce::{Capabilities, Decision, HandlerAnswer, Question, Reason};
use hexput_shared::diagnostics::Span;

fn span() -> Span {
    Span::new(4, 11, 1, 5)
}

fn session() -> Capabilities {
    Capabilities::registered([("getOrder", true), ("sendMail", false)])
}

/// The question `check_call` asks about `name`, which must be registered without a grant.
fn question(capabilities: &Capabilities, name: &str) -> Question {
    match capabilities.check_call(name, span()) {
        Ok(Decision::AskHandler(question)) => question,
        other => panic!("expected a question for `{name}`, got {other:?}"),
    }
}

#[test]
fn a_blanket_granted_function_may_be_called_without_asking() {
    assert!(matches!(
        session().check_call("getOrder", span()),
        Ok(Decision::Allowed)
    ));
}

#[test]
fn a_registered_function_without_a_grant_asks_the_handler() {
    let question = question(&session(), "sendMail");
    assert_eq!(question.name(), "sendMail");
}

#[test]
fn only_an_explicit_true_allows_the_call() {
    assert_eq!(
        question(&session(), "sendMail").decide(HandlerAnswer::Boolean(true)),
        Ok(())
    );
}

#[test]
fn every_other_answer_is_refused_with_its_own_reason() {
    let cases = [
        (HandlerAnswer::Boolean(false), Reason::Refused, "refused"),
        (
            HandlerAnswer::NotBoolean,
            Reason::HandlerInvalid,
            "handler_invalid",
        ),
        (
            HandlerAnswer::Failed,
            Reason::HandlerFailed,
            "handler_failed",
        ),
        (
            HandlerAnswer::TimedOut,
            Reason::HandlerTimeout,
            "handler_timeout",
        ),
        (
            HandlerAnswer::NoReply,
            Reason::HandlerNoReply,
            "handler_no_reply",
        ),
    ];
    let unregistered = session()
        .check_call("nope", span())
        .unwrap_err()
        .into_diagnostic();
    for (answer, reason, spelled) in cases {
        // `nope` registered without a grant in one Session, absent from the other: the Script
        // sees exactly the same diagnostic.
        let refusal = question(&Capabilities::registered([("nope", false)]), "nope")
            .decide(answer)
            .unwrap_err();
        assert_eq!(refusal.reason(), reason, "{answer:?}");
        assert_eq!(refusal.reason().as_str(), spelled);
        let error = refusal.diagnostic();
        assert_eq!(error.category.as_str(), "capability");
        assert_eq!(error.code.as_str(), "capability.unknown_function");
        assert_eq!(error.span, span());
        // Identical to an unregistered name's error: code, message and span.
        assert_eq!(refusal.into_diagnostic(), unregistered, "{answer:?}");
    }
}

#[test]
fn an_unregistered_function_is_refused_as_unregistered() {
    let refusal = session().check_call("nope", span()).unwrap_err();
    assert_eq!(refusal.reason(), Reason::Unregistered);
    assert_eq!(refusal.reason().as_str(), "unregistered");
    let error = refusal.diagnostic();
    assert_eq!(error.category.as_str(), "capability");
    assert_eq!(error.code.as_str(), "capability.unknown_function");
    assert_eq!(error.span, span());
}

#[test]
fn every_call_asks_again() {
    // Nothing is cached: the same name yields a fresh question each time.
    let capabilities = session();
    assert_eq!(
        question(&capabilities, "sendMail").decide(HandlerAnswer::Boolean(true)),
        Ok(())
    );
    assert!(
        question(&capabilities, "sendMail")
            .decide(HandlerAnswer::Boolean(false))
            .is_err()
    );
}

#[test]
fn nothing_is_callable_with_no_capabilities() {
    let refusal = Capabilities::none()
        .check_call("getOrder", span())
        .unwrap_err();
    assert_eq!(refusal.reason(), Reason::Unregistered);
}

#[test]
fn a_name_listed_twice_is_blanket_granted_only_if_every_listing_grants_it() {
    for listed in [[("f", false), ("f", true)], [("f", true), ("f", false)]] {
        let decision = Capabilities::registered(listed).check_call("f", span());
        assert!(
            matches!(decision, Ok(Decision::AskHandler(_))),
            "{listed:?}: {decision:?}"
        );
    }
    assert!(matches!(
        Capabilities::registered([("f", true), ("f", true)]).check_call("f", span()),
        Ok(Decision::Allowed)
    ));
}

// --- Story 3.5: the CPU time and memory dimensions of the Resource Budget ---

mod budget {
    use std::time::Duration;

    use hexput_enforce::{Budget, DEFAULT_CPU_TIME, DEFAULT_MEMORY, Dimension};

    use super::span;

    #[test]
    fn the_defaults_are_one_second_and_sixty_four_mebibytes() {
        assert_eq!(DEFAULT_CPU_TIME, Duration::from_secs(1));
        assert_eq!(DEFAULT_MEMORY, 64 * 1024 * 1024);
        let budget = Budget::new();
        assert_eq!(budget.limits().cpu_time(), DEFAULT_CPU_TIME);
        assert_eq!(budget.limits().memory(), DEFAULT_MEMORY);
        assert_eq!(budget.memory_ceiling(), DEFAULT_MEMORY);
        assert_eq!(budget.cpu_used(), Duration::ZERO);
    }

    #[test]
    fn cpu_time_adds_up_and_the_charge_that_passes_the_limit_is_refused() {
        let mut budget = Budget::new();
        for _ in 0..4 {
            budget
                .charge_cpu(Duration::from_millis(250), span())
                .unwrap();
        }
        assert_eq!(
            budget.cpu_used(),
            DEFAULT_CPU_TIME,
            "exactly at the limit is within it"
        );
        let exceeded = budget
            .charge_cpu(Duration::from_millis(1), span())
            .unwrap_err();
        assert_eq!(exceeded.dimension(), Dimension::CpuTime);
        let diagnostic = exceeded.diagnostic();
        assert_eq!(diagnostic.code.as_str(), "budget.cpu_time_exceeded");
        assert_eq!(diagnostic.category.as_str(), "budget");
        assert_eq!(diagnostic.span, span());
        assert!(
            diagnostic.message.contains("1000 ms"),
            "{}",
            diagnostic.message
        );
    }

    #[test]
    fn the_memory_error_names_its_dimension_and_is_spanned() {
        let exceeded = Budget::new().memory_exceeded(span());
        assert_eq!(exceeded.dimension(), Dimension::Memory);
        let diagnostic = exceeded.into_diagnostic();
        assert_eq!(diagnostic.code.as_str(), "budget.memory_exceeded");
        assert_eq!(diagnostic.category.as_str(), "budget");
        assert_eq!(diagnostic.span, span());
    }

    #[test]
    fn every_dimension_has_a_stable_distinct_name() {
        let mut names: Vec<_> = Dimension::ALL.iter().map(|d| d.as_str()).collect();
        assert_eq!(names.len(), 6);
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), 6);
        assert_eq!(Dimension::CpuTime.to_string(), "cpu_time");
    }
}
