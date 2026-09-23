#[derive(Debug, Clone)]
pub struct TableOptions {
    pub columns: Vec<String>,
    pub no_headers: bool,
    pub no_truncate: bool,
    pub interactive: bool,
    pub max_width: usize,
}

impl Default for TableOptions {
    fn default() -> Self {
        TableOptions { columns: Vec::new(), no_headers: false, no_truncate: false, interactive: true, max_width: 80 }
    }
}

fn select_columns(headers: &[&str], rows: &[Vec<String>], wanted: &[String]) -> (Vec<String>, Vec<Vec<String>>) {
    if wanted.is_empty() {
        return (headers.iter().map(|h| h.to_string()).collect(), rows.to_vec());
    }
    let idx: Vec<usize> = wanted.iter().filter_map(|w| headers.iter().position(|h| h.eq_ignore_ascii_case(w))).collect();
    let sel_headers = idx.iter().map(|&i| headers[i].to_string()).collect();
    let sel_rows = rows.iter().map(|r| idx.iter().map(|&i| r.get(i).cloned().unwrap_or_default()).collect()).collect();
    (sel_headers, sel_rows)
}

fn is_numeric_column(rows: &[Vec<String>], i: usize) -> bool {
    !rows.is_empty() && rows.iter().all(|r| r.get(i).is_some_and(|c| !c.is_empty() && c.chars().all(|ch| ch.is_ascii_digit())))
}

fn pad(width: usize, numeric: bool, cell: &str) -> String {
    let fill = " ".repeat(width.saturating_sub(cell.chars().count()));
    if numeric {
        format!("{fill}{cell}")
    } else {
        format!("{cell}{fill}")
    }
}

pub fn render_with(headers: &[&str], rows: &[Vec<String>], opts: &TableOptions) -> String {
    let (sel_headers, sel_rows) = select_columns(headers, rows, &opts.columns);
    let sel_rows: Vec<Vec<String>> = sel_rows
        .into_iter()
        .map(|row| row.into_iter().map(|c| if !opts.interactive && c == "—" { "-".to_string() } else { c }).collect())
        .collect();
    let numeric: Vec<bool> = (0..sel_headers.len()).map(|i| is_numeric_column(&sel_rows, i)).collect();
    let mut widths: Vec<usize> = sel_headers.iter().map(|h| h.chars().count()).collect();
    for row in &sel_rows {
        for (i, cell) in row.iter().enumerate() {
            if i < widths.len() {
                widths[i] = widths[i].max(cell.chars().count());
            }
        }
    }
    let line = |cells: &[String]| -> String {
        let padded: Vec<String> = cells.iter().zip(&widths).zip(&numeric).map(|((c, w), n)| pad(*w, *n, c)).collect();
        padded.join("  ").trim_end().to_string()
    };
    let clip = |s: String| -> String {
        if opts.interactive && !opts.no_truncate && s.chars().count() > opts.max_width {
            let mut t: String = s.chars().take(opts.max_width.saturating_sub(1)).collect();
            t.push('…');
            t
        } else {
            s
        }
    };
    let mut out = String::new();
    if !opts.no_headers {
        out.push_str(&clip(line(&sel_headers)));
        out.push('\n');
    }
    for row in &sel_rows {
        out.push_str(&clip(line(row)));
        out.push('\n');
    }
    out
}

pub fn render(headers: &[&str], rows: &[Vec<String>]) -> String {
    render_with(headers, rows, &TableOptions::default())
}

pub fn empty_state(message: &str) -> String {
    format!("{message}\n")
}
