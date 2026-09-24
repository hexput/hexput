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

// --- Story 3.6: allocations, RPC calls, output size and side effects ---

mod counted {
    use std::time::Duration;

    use hexput_enforce::{
        Budget, DEFAULT_ALLOCATIONS, DEFAULT_OUTPUT_SIZE, DEFAULT_RPC_CALLS, DEFAULT_SIDE_EFFECTS,
        Dimension, Exceeded, Limits,
    };

    use super::span;

    fn assert_dimension(exceeded: &Exceeded, dimension: Dimension, code: &str) {
        assert_eq!(exceeded.dimension(), dimension);
        let diagnostic = exceeded.diagnostic();
        assert_eq!(diagnostic.code.as_str(), code);
        assert_eq!(diagnostic.category.as_str(), "budget");
        assert_eq!(diagnostic.span, span());
    }

    #[test]
    fn the_defaults_are_the_decided_ones() {
        assert_eq!(DEFAULT_ALLOCATIONS, 1_000_000);
        assert_eq!(DEFAULT_RPC_CALLS, 100);
        assert_eq!(DEFAULT_OUTPUT_SIZE, 1024 * 1024);
        assert_eq!(DEFAULT_SIDE_EFFECTS, 100);
        let limits = Budget::new().limits();
        assert_eq!(limits.allocations(), DEFAULT_ALLOCATIONS);
        assert_eq!(limits.rpc_calls(), DEFAULT_RPC_CALLS);
        assert_eq!(limits.output_size(), DEFAULT_OUTPUT_SIZE);
        assert_eq!(limits.side_effects(), DEFAULT_SIDE_EFFECTS);
        assert_eq!(Budget::new().allocation_ceiling(), DEFAULT_ALLOCATIONS);
    }

    #[test]
    fn a_host_call_is_one_rpc_call_and_one_side_effect_and_the_one_past_the_limit_is_refused() {
        let mut budget = Budget::new();
        for _ in 0..DEFAULT_RPC_CALLS {
            budget.charge_rpc_call(span()).unwrap();
        }
        assert_eq!(budget.rpc_calls_used(), 100);
        assert_eq!(budget.side_effects_used(), 100);
        let exceeded = budget.charge_rpc_call(span()).unwrap_err();
        assert_dimension(&exceeded, Dimension::RpcCalls, "budget.rpc_calls_exceeded");
        assert_eq!(
            budget.rpc_calls_used(),
            100,
            "the refused call is not charged"
        );
    }

    #[test]
    fn output_size_is_refused_only_past_the_limit() {
        let budget = Budget::new();
        budget.charge_output(DEFAULT_OUTPUT_SIZE, span()).unwrap();
        let exceeded = budget
            .charge_output(DEFAULT_OUTPUT_SIZE + 1, span())
            .unwrap_err();
        assert_dimension(
            &exceeded,
            Dimension::OutputSize,
            "budget.output_size_exceeded",
        );
    }

    #[test]
    fn the_allocation_error_names_its_dimension() {
        let exceeded = Budget::new().allocations_exceeded(span());
        assert_dimension(
            &exceeded,
            Dimension::Allocations,
            "budget.allocations_exceeded",
        );
    }

    #[test]
    fn each_limit_crossed_alone_names_only_its_own_dimension() {
        // Side effects below RPC calls: the side-effect limit is what the call crosses.
        let mut budget = Budget::with_limits(Limits::default().with_side_effects(3));
        for _ in 0..3 {
            budget.charge_rpc_call(span()).unwrap();
        }
        let exceeded = budget.charge_rpc_call(span()).unwrap_err();
        assert_dimension(
            &exceeded,
            Dimension::SideEffects,
            "budget.side_effects_exceeded",
        );

        // RPC calls below side effects: the RPC call limit is.
        let mut budget = Budget::with_limits(Limits::default().with_rpc_calls(2));
        for _ in 0..2 {
            budget.charge_rpc_call(span()).unwrap();
        }
        let exceeded = budget.charge_rpc_call(span()).unwrap_err();
        assert_dimension(&exceeded, Dimension::RpcCalls, "budget.rpc_calls_exceeded");

        // Tight limits on every other dimension leave these two untouched, and the reverse.
        let tight = Limits::default()
            .with_cpu_time(Duration::from_millis(1))
            .with_memory(1)
            .with_allocations(1)
            .with_output_size(1);
        let mut budget = Budget::with_limits(tight);
        for _ in 0..DEFAULT_RPC_CALLS {
            budget.charge_rpc_call(span()).unwrap();
        }
        // Only output size and CPU time tightened: host calls still go ahead.
        let mut budget = Budget::with_limits(
            Limits::default()
                .with_output_size(0)
                .with_cpu_time(Duration::ZERO),
        );
        for _ in 0..DEFAULT_RPC_CALLS {
            budget.charge_rpc_call(span()).unwrap();
        }
        assert_eq!(budget.rpc_calls_used(), DEFAULT_RPC_CALLS);
        let budget = Budget::with_limits(
            Limits::default()
                .with_rpc_calls(0)
                .with_side_effects(0)
                .with_allocations(0),
        );
        budget.charge_output(DEFAULT_OUTPUT_SIZE, span()).unwrap();
        assert_eq!(budget.memory_ceiling(), hexput_enforce::DEFAULT_MEMORY);
        let mut budget = budget;
        budget
            .charge_cpu(hexput_enforce::DEFAULT_CPU_TIME, span())
            .unwrap();
    }
}

// --- Story 3.7: limits from settings ---

mod limits {
    use std::time::Duration;

    use hexput_enforce::{Limits, Setting, Settings};

    #[test]
    fn no_setting_is_the_documented_defaults() {
        let limits = Limits::from_settings(&Settings::new());
        assert_eq!(limits, Limits::default());
        assert_eq!(limits.cpu_time(), Duration::from_secs(1));
        assert_eq!(limits.memory(), 64 * 1024 * 1024);
        assert_eq!(limits.allocations(), 1_000_000);
        assert_eq!(limits.rpc_calls(), 100);
        assert_eq!(limits.output_size(), 1024 * 1024);
        assert_eq!(limits.side_effects(), 100);
        assert_eq!(limits.argument_depth(), 12);
        assert_eq!(limits.authorization_timeout(), Duration::from_secs(5));
    }

    #[test]
    fn every_setting_reaches_its_limit() {
        let mut settings = Settings::new();
        for (setting, value) in [
            (Setting::CpuTimeMs, 250),
            (Setting::MemoryBytes, 4096),
            (Setting::Allocations, 3),
            (Setting::RpcCalls, 0),
            (Setting::OutputSizeBytes, 77),
            (Setting::SideEffects, 9),
            (Setting::ArgumentDepth, 2),
            (Setting::AuthorizationTimeoutMs, 100),
        ] {
            settings.set(setting, value).unwrap();
        }
        let expected = Limits::default()
            .with_cpu_time(Duration::from_millis(250))
            .with_memory(4096)
            .with_allocations(3)
            .with_rpc_calls(0)
            .with_output_size(77)
            .with_side_effects(9)
            .with_argument_depth(2)
            .with_authorization_timeout(Duration::from_millis(100));
        assert_eq!(Limits::from_settings(&settings), expected);
    }
}
