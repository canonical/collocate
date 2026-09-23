use collocate_cli::output::{confirm_policy, parse_answer, resolve_verbosity, should_emit, success_line, ConfirmPolicy, Verbosity};

#[test]
fn verbosity_orders_quiet_below_everything() {
    assert!(Verbosity::Quiet < Verbosity::Brief);
    assert!(Verbosity::Brief < Verbosity::Verbose);
    assert!(Verbosity::Verbose < Verbosity::Debug);
    assert!(Verbosity::Debug < Verbosity::Trace);
}

#[test]
fn should_emit_suppresses_narration_when_quiet() {
    assert!(!should_emit(Verbosity::Quiet, Verbosity::Brief));
    assert!(should_emit(Verbosity::Brief, Verbosity::Brief));
    assert!(should_emit(Verbosity::Verbose, Verbosity::Brief));
    assert!(!should_emit(Verbosity::Brief, Verbosity::Verbose));
    assert!(should_emit(Verbosity::Trace, Verbosity::Debug));
}

#[test]
fn resolve_verbosity_prefers_explicit_value() {
    assert_eq!(resolve_verbosity(Some(Verbosity::Debug), true, 3), Verbosity::Debug);
}

#[test]
fn resolve_verbosity_quiet_flag_wins_over_verbose_count() {
    assert_eq!(resolve_verbosity(None, true, 2), Verbosity::Quiet);
}

#[test]
fn resolve_verbosity_counts_repeated_verbose_flags() {
    assert_eq!(resolve_verbosity(None, false, 0), Verbosity::Brief);
    assert_eq!(resolve_verbosity(None, false, 1), Verbosity::Verbose);
    assert_eq!(resolve_verbosity(None, false, 2), Verbosity::Debug);
    assert_eq!(resolve_verbosity(None, false, 3), Verbosity::Trace);
    assert_eq!(resolve_verbosity(None, false, 9), Verbosity::Trace);
}

#[test]
fn success_line_states_the_verb_and_the_subject() {
    assert_eq!(success_line("Started", "web"), "Started web.");
    assert_eq!(success_line("Deleted", "demo-db-1"), "Deleted demo-db-1.");
}

#[test]
fn confirm_policy_yes_flag_always_wins() {
    assert_eq!(confirm_policy(true, true), ConfirmPolicy::AutoYes);
    assert_eq!(confirm_policy(true, false), ConfirmPolicy::AutoYes);
}

#[test]
fn confirm_policy_asks_only_on_a_real_terminal() {
    assert_eq!(confirm_policy(false, true), ConfirmPolicy::Ask);
    assert_eq!(confirm_policy(false, false), ConfirmPolicy::RefuseNonInteractive);
}

#[test]
fn parse_answer_accepts_yes_and_no_case_insensitively() {
    for yes in ["y", "Y", "yes", "YES", "Yes"] {
        assert_eq!(parse_answer(yes), Some(true), "{yes}");
    }
    for no in ["n", "N", "no", "NO"] {
        assert_eq!(parse_answer(no), Some(false), "{no}");
    }
}

#[test]
fn parse_answer_trims_whitespace_and_the_trailing_newline() {
    assert_eq!(parse_answer("y\n"), Some(true));
    assert_eq!(parse_answer("  no  \n"), Some(false));
}

#[test]
fn parse_answer_is_none_for_empty_or_unrecognized_input() {
    assert_eq!(parse_answer(""), None);
    assert_eq!(parse_answer("\n"), None);
    assert_eq!(parse_answer("maybe"), None);
}
