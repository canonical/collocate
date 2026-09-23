use collocate_cli::status::{format_ports, format_process, format_uptime, main_process, ProcEntry};
use collocate_cli::table::{empty_state, render, render_with, TableOptions};
use collocate_core::procinfo::ProcState;
use std::time::Duration;

fn p(pid: u32, ppid: u32, comm: &str, state: ProcState) -> ProcEntry {
    ProcEntry { pid, ppid, comm: comm.into(), state }
}

#[test]
fn table_pads_columns_and_trims_trailing_space() {
    let out = render(&["NAME", "STATE"], &[vec!["db".into(), "running".into()], vec!["longer-name".into(), "stopped".into()]]);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines[0], "NAME         STATE");
    assert_eq!(lines[1], "db           running");
    assert_eq!(lines[2], "longer-name  stopped");
    assert!(lines.iter().all(|l| !l.ends_with(' ')));
}

#[test]
fn table_handles_wide_unicode_placeholders_and_empty_input() {
    let out = render(&["A", "B"], &[vec!["—".into(), "x".into()]]);
    assert!(out.lines().nth(1).unwrap().starts_with("—"));
    assert_eq!(render(&["A"], &[]).lines().count(), 1);
}

#[test]
fn the_main_process_is_the_child_of_init() {
    let procs = vec![
        p(100, 1, "init", ProcState::Sleeping),
        p(101, 100, "postgres", ProcState::Running),
        p(150, 101, "worker", ProcState::Sleeping),
        p(151, 101, "worker", ProcState::Sleeping),
    ];
    let main = main_process(&procs).unwrap();
    assert_eq!(main.pid, 101);
    assert_eq!(format_process(&procs), "postgres (R, 101)+2");
}

#[test]
fn single_process_has_no_suffix_and_empty_has_none() {
    let procs = vec![p(100, 1, "init", ProcState::Sleeping), p(101, 100, "myserver", ProcState::Sleeping)];
    assert_eq!(format_process(&procs), "myserver (S, 101)");
    assert!(main_process(&[]).is_none());
    assert_eq!(format_process(&[]), "—");
    assert_eq!(format_process(&[p(100, 1, "init", ProcState::Sleeping)]), "init (S, 100)");
}

#[test]
fn ports_are_formatted_by_protocol() {
    assert_eq!(format_ports(&[5432], &[]), "5432/tcp");
    assert_eq!(format_ports(&[80, 8080], &[53]), "80/tcp,8080/tcp,53/udp");
    assert_eq!(format_ports(&[], &[]), "—");
}

#[test]
fn uptime_uses_the_two_largest_units() {
    assert_eq!(format_uptime(Duration::from_secs(45)), "45s");
    assert_eq!(format_uptime(Duration::from_secs(3 * 60 + 5)), "3m5s");
    assert_eq!(format_uptime(Duration::from_secs(2 * 3600 + 14 * 60 + 9)), "2h14m");
    assert_eq!(format_uptime(Duration::from_secs(3 * 86400 + 2 * 3600)), "3d2h");
    assert_eq!(format_uptime(Duration::ZERO), "0s");
}

fn rows() -> Vec<Vec<String>> {
    vec![vec!["web".into(), "running".into(), "2".into()], vec!["db".into(), "stopped".into(), "10".into()]]
}

#[test]
fn columns_option_selects_and_reorders_a_subset() {
    let opts = TableOptions { columns: vec!["LAYERS".into(), "NAME".into()], ..TableOptions::default() };
    let out = render_with(&["NAME", "STATE", "LAYERS"], &rows(), &opts);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines[0], "LAYERS  NAME");
    assert_eq!(lines[1], "     2  web");
    assert_eq!(lines[2], "    10  db");
}

#[test]
fn column_selection_is_case_insensitive_and_ignores_unknown_names() {
    let opts = TableOptions { columns: vec!["name".into(), "bogus".into()], ..TableOptions::default() };
    let out = render_with(&["NAME", "STATE"], &[vec!["web".into(), "running".into()]], &opts);
    assert_eq!(out.lines().next().unwrap(), "NAME");
}

#[test]
fn no_headers_suppresses_the_header_row() {
    let opts = TableOptions { no_headers: true, ..TableOptions::default() };
    let out = render_with(&["NAME", "STATE"], &[vec!["web".into(), "running".into()]], &opts);
    assert_eq!(out.lines().count(), 1);
    assert_eq!(out.lines().next().unwrap(), "web   running");
}

#[test]
fn numeric_columns_are_right_aligned_including_the_header() {
    let opts = TableOptions::default();
    let out = render_with(&["A", "N"], &[vec!["x".into(), "7".into()], vec!["y".into(), "42".into()]], &opts);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines[0], "A   N");
    assert_eq!(lines[1], "x   7");
    assert_eq!(lines[2], "y  42");
}

#[test]
fn a_column_with_any_non_digit_cell_is_left_aligned() {
    let opts = TableOptions::default();
    let out = render_with(&["NAME", "ADDRESS"], &[vec!["web".into(), "172.30.0.2".into()]], &opts);
    assert_eq!(out.lines().nth(1).unwrap(), "web   172.30.0.2");
}

#[test]
fn non_interactive_output_uses_an_ascii_dash_for_placeholders() {
    let opts = TableOptions { interactive: false, ..TableOptions::default() };
    let out = render_with(&["NAME", "ADDRESS"], &[vec!["web".into(), "—".into()]], &opts);
    assert!(out.contains(" -"), "{out:?}");
    assert!(!out.contains('—'), "{out:?}");
}

#[test]
fn interactive_output_keeps_the_em_dash_placeholder() {
    let opts = TableOptions { interactive: true, ..TableOptions::default() };
    let out = render_with(&["NAME", "ADDRESS"], &[vec!["web".into(), "—".into()]], &opts);
    assert!(out.contains('—'));
}

#[test]
fn long_rows_are_truncated_when_interactive_unless_no_truncate_is_set() {
    let long = "x".repeat(50);
    let opts = TableOptions { max_width: 20, ..TableOptions::default() };
    let out = render_with(&["NAME"], &[vec![long.clone()]], &opts);
    let line = out.lines().nth(1).unwrap();
    assert!(line.chars().count() <= 20, "{line:?}");
    assert!(line.ends_with('…'));

    let untruncated = TableOptions { max_width: 20, no_truncate: true, ..TableOptions::default() };
    let out = render_with(&["NAME"], &[vec![long.clone()]], &untruncated);
    assert_eq!(out.lines().nth(1).unwrap(), long);
}

#[test]
fn non_interactive_output_is_never_truncated() {
    let long = "x".repeat(200);
    let opts = TableOptions { interactive: false, max_width: 20, ..TableOptions::default() };
    let out = render_with(&["NAME"], &[vec![long.clone()]], &opts);
    assert_eq!(out.lines().nth(1).unwrap(), long);
}

#[test]
fn render_matches_render_with_default_interactive_options() {
    let sample = [vec!["web".to_string(), "running".to_string()]];
    assert_eq!(render(&["NAME", "STATE"], &sample), render_with(&["NAME", "STATE"], &sample, &TableOptions::default()));
}

#[test]
fn empty_state_is_a_single_specific_message() {
    assert_eq!(empty_state("No containers found."), "No containers found.\n");
}
