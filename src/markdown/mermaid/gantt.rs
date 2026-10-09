//! Gantt charts, drawn as one row per task with bars scaled to a shared date
//! axis. Times are naive (no time zone) milliseconds since the Unix epoch.

use std::fmt::Write;

use crate::markdown::width::{display_width, truncate_display_width};

const SECOND: i64 = 1_000;
const MINUTE: i64 = 60 * SECOND;
const HOUR: i64 = 60 * MINUTE;
const DAY: i64 = 24 * HOUR;
const WEEK: i64 = 7 * DAY;

const MIN_CHART_WIDTH: usize = 10;
const MAX_CHART_WIDTH: usize = 80;
const LABEL_GAP: usize = 1;

const BAR: char = '█';
const BAR_ACTIVE: char = '▓';
const BAR_DONE: char = '░';
const BAR_CRIT: char = '▚';
const MILESTONE: char = '◆';

const MONTH_NAMES: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
const WEEKDAY_NAMES: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];

pub(super) fn render(content: &str, max_width: usize) -> Option<String> {
    let chart = parse(content)?;
    let tasks = chart.resolve()?;
    draw(&chart, &tasks, max_width)
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Tags {
    active: bool,
    done: bool,
    crit: bool,
    milestone: bool,
}

#[derive(Debug, Clone, PartialEq)]
enum Start {
    Previous,
    At(i64),
    After(Vec<String>),
}

#[derive(Debug, Clone, PartialEq)]
enum End {
    At(i64),
    Duration(Duration),
    Until(Vec<String>),
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Duration {
    Millis(i64),
    Months(f64),
}

impl Duration {
    fn add_to(self, time: i64) -> i64 {
        match self {
            Duration::Millis(ms) => time + ms,
            Duration::Months(months) => {
                let whole = months.trunc() as i64;
                let shifted = add_months(time, whole);
                let fraction = months.fract();
                shifted + (fraction * 30.0 * DAY as f64) as i64
            }
        }
    }
}

#[derive(Debug)]
struct Task {
    name: String,
    section: Option<usize>,
    id: Option<String>,
    tags: Tags,
    start: Start,
    end: End,
}

#[derive(Debug, Clone, Copy)]
struct Placed {
    start: i64,
    /// End used by dependent tasks, extended over excluded days.
    end: i64,
    /// End drawn on the chart, ignoring trailing excluded days.
    render_end: i64,
}

#[derive(Debug)]
struct Gantt {
    title: Option<String>,
    date_format: String,
    axis_format: String,
    tick_interval: Option<Duration>,
    inclusive_end_dates: bool,
    excluded_weekdays: Vec<u32>,
    excluded_days: Vec<i64>,
    sections: Vec<String>,
    tasks: Vec<Task>,
}

impl Default for Gantt {
    fn default() -> Self {
        Gantt {
            title: None,
            date_format: "YYYY-MM-DD".to_string(),
            axis_format: "%Y-%m-%d".to_string(),
            tick_interval: None,
            inclusive_end_dates: false,
            excluded_weekdays: Vec::new(),
            excluded_days: Vec::new(),
            sections: Vec::new(),
            tasks: Vec::new(),
        }
    }
}

fn parse(content: &str) -> Option<Gantt> {
    let mut lines = content.lines();
    if lines.next()?.trim() != "gantt" {
        return None;
    }

    let mut chart = Gantt::default();
    let mut excludes = Vec::new();
    let mut weekend_start = 6; // Saturday

    for source_line in lines {
        let line = source_line.trim();
        if line.is_empty() || line.starts_with("%%") {
            continue;
        }
        let (keyword, rest) = match line.split_once(char::is_whitespace) {
            Some((keyword, rest)) => (keyword, rest.trim()),
            None => (line, ""),
        };
        match keyword {
            "title" => chart.title = Some(rest.to_string()),
            "dateFormat" => chart.date_format = rest.to_string(),
            "axisFormat" => chart.axis_format = rest.to_string(),
            "tickInterval" => chart.tick_interval = parse_tick_interval(rest),
            "inclusiveEndDates" => chart.inclusive_end_dates = true,
            "excludes" => excludes.push(rest.to_string()),
            "weekend" => {
                weekend_start = if rest.eq_ignore_ascii_case("friday") {
                    5
                } else {
                    6
                }
            }
            "section" => chart.sections.push(rest.to_string()),
            "includes" | "todayMarker" | "weekday" | "topAxis" | "displayMode" | "accTitle"
            | "accTitle:" | "accDescr" | "accDescr:" | "click" | "vert" => {}
            _ => {
                if let Some(task) = parse_task(line, &chart) {
                    chart.tasks.push(task);
                }
            }
        }
    }

    for exclusion in excludes
        .iter()
        .flat_map(|list| list.split([',', ' ']))
        .map(str::trim)
        .filter(|item| !item.is_empty())
    {
        let lower = exclusion.to_ascii_lowercase();
        if lower == "weekends" {
            chart
                .excluded_weekdays
                .extend([weekend_start, (weekend_start + 1) % 7]);
        } else if let Some(day) = WEEKDAY_NAMES
            .iter()
            .position(|name| name.eq_ignore_ascii_case(&lower))
        {
            chart.excluded_weekdays.push(day as u32);
        } else if let Some(date) = parse_date(exclusion, &chart.date_format) {
            chart.excluded_days.push(date.div_euclid(DAY));
        }
    }

    if chart.tasks.is_empty() {
        return None;
    }
    Some(chart)
}

fn parse_task(line: &str, chart: &Gantt) -> Option<Task> {
    let (name, metadata) = line.split_once(':')?;
    let name = name.trim();
    if name.is_empty() {
        return None;
    }

    let mut items: Vec<&str> = metadata.split(',').map(str::trim).collect();
    let mut tags = Tags::default();
    while let Some(first) = items.first() {
        match *first {
            "active" => tags.active = true,
            "done" => tags.done = true,
            "crit" => tags.crit = true,
            "milestone" => tags.milestone = true,
            _ => break,
        }
        items.remove(0);
    }

    let (id, start, end) = match items.as_slice() {
        [end] => (None, Start::Previous, *end),
        [start, end] => (None, parse_start(start, chart)?, *end),
        [id, start, end] => (Some(id.to_string()), parse_start(start, chart)?, *end),
        _ => return None,
    };

    Some(Task {
        name: name.to_string(),
        section: chart.sections.len().checked_sub(1),
        id,
        tags,
        start,
        end: parse_end(end, chart)?,
    })
}

fn parse_start(text: &str, chart: &Gantt) -> Option<Start> {
    if let Some(ids) = text.strip_prefix("after ") {
        return Some(Start::After(
            ids.split_whitespace().map(str::to_string).collect(),
        ));
    }
    parse_date(text, &chart.date_format).map(Start::At)
}

fn parse_end(text: &str, chart: &Gantt) -> Option<End> {
    if let Some(ids) = text.strip_prefix("until ") {
        return Some(End::Until(
            ids.split_whitespace().map(str::to_string).collect(),
        ));
    }
    if let Some(date) = parse_date(text, &chart.date_format) {
        let date = if chart.inclusive_end_dates {
            date + DAY
        } else {
            date
        };
        return Some(End::At(date));
    }
    parse_duration(text).map(End::Duration)
}

/// `30d`, `1.5w`, `2h`, `90m`, `500ms`, `1M`, `1y`.
fn parse_duration(text: &str) -> Option<Duration> {
    let split = text
        .find(|ch: char| !(ch.is_ascii_digit() || ch == '.'))
        .filter(|&split| split > 0)?;
    let value: f64 = text[..split].parse().ok()?;
    let millis = |unit: i64| Some(Duration::Millis((value * unit as f64).round() as i64));
    match &text[split..] {
        "ms" => millis(1),
        "s" => millis(SECOND),
        "m" => millis(MINUTE),
        "h" => millis(HOUR),
        "d" => millis(DAY),
        "w" => millis(WEEK),
        "M" => Some(Duration::Months(value)),
        "y" => Some(Duration::Months(value * 12.0)),
        _ => None,
    }
}

/// `tickInterval 1week`, `2day`, `6hour`, `1month`.
fn parse_tick_interval(text: &str) -> Option<Duration> {
    let split = text.find(|ch: char| !ch.is_ascii_digit())?;
    let value: i64 = text[..split].parse().ok().filter(|&value| value > 0)?;
    let millis = |unit: i64| Some(Duration::Millis(value * unit));
    match &text[split..] {
        "millisecond" => millis(1),
        "second" => millis(SECOND),
        "minute" => millis(MINUTE),
        "hour" => millis(HOUR),
        "day" => millis(DAY),
        "week" => millis(WEEK),
        "month" => Some(Duration::Months(value as f64)),
        _ => None,
    }
}

impl Gantt {
    /// Place every task on the time line. Tasks may refer to tasks declared
    /// later, so placement repeats until nothing changes.
    fn resolve(&self) -> Option<Vec<Placed>> {
        let mut placed: Vec<Option<Placed>> = vec![None; self.tasks.len()];
        loop {
            let mut progressed = false;
            for (index, task) in self.tasks.iter().enumerate() {
                if placed[index].is_some() {
                    continue;
                }
                if let Some(position) = self.place(task, index, &placed) {
                    placed[index] = Some(position);
                    progressed = true;
                }
            }
            if !progressed {
                break;
            }
        }
        placed.into_iter().collect()
    }

    fn place(&self, task: &Task, index: usize, placed: &[Option<Placed>]) -> Option<Placed> {
        let start = match &task.start {
            Start::At(time) => *time,
            Start::Previous => placed[index.checked_sub(1)?]?.end,
            Start::After(ids) => self.lookup(ids, placed, |p| p.end)?.max()?,
        };
        let (end, from_duration) = match &task.end {
            End::At(time) => (*time, false),
            End::Duration(duration) => (duration.add_to(start), true),
            End::Until(ids) => (self.lookup(ids, placed, |p| p.start)?.min()?, false),
        };
        let end = end.max(start);
        if !from_duration || (self.excluded_weekdays.is_empty() && self.excluded_days.is_empty()) {
            return Some(Placed {
                start,
                end,
                render_end: end,
            });
        }

        // Mermaid stretches computed ends over excluded days, but draws the
        // bar only up to the last working day.
        let mut day = start;
        let mut end = end;
        let mut render_end = end;
        let mut excluded = false;
        while day <= end {
            if !excluded {
                render_end = end;
            }
            excluded = self.is_excluded(day);
            if excluded {
                end += DAY;
            }
            day += DAY;
        }
        Some(Placed {
            start,
            end,
            render_end,
        })
    }

    fn lookup(
        &self,
        ids: &[String],
        placed: &[Option<Placed>],
        pick: fn(&Placed) -> i64,
    ) -> Option<std::vec::IntoIter<i64>> {
        let mut times = Vec::new();
        for id in ids {
            let index = self
                .tasks
                .iter()
                .position(|task| task.id.as_deref() == Some(id.as_str()))?;
            times.push(pick(placed[index].as_ref()?));
        }
        Some(times.into_iter())
    }

    fn is_excluded(&self, time: i64) -> bool {
        let day = time.div_euclid(DAY);
        self.excluded_days.contains(&day) || self.excluded_weekdays.contains(&weekday(day))
    }
}

fn draw(chart: &Gantt, tasks: &[Placed], max_width: usize) -> Option<String> {
    let indent = if chart.sections.is_empty() { "" } else { "  " };
    let widest_name = chart
        .tasks
        .iter()
        .map(|task| display_width(&task.name))
        .max()
        .unwrap_or(0);
    let mut label_width = (display_width(indent) + widest_name).min(max_width * 2 / 5);
    if max_width < label_width + LABEL_GAP + MIN_CHART_WIDTH {
        label_width = max_width.checked_sub(LABEL_GAP + MIN_CHART_WIDTH)?;
    }
    if label_width < display_width(indent) + 3 {
        return None;
    }
    let chart_width = (max_width - label_width - LABEL_GAP).min(MAX_CHART_WIDTH);

    let low = tasks.iter().map(|task| task.start).min()?;
    let high = tasks.iter().map(|task| task.render_end).max()?;
    let high = if high > low { high } else { low + DAY };
    let column = |time: i64| (time - low) as f64 * chart_width as f64 / (high - low) as f64;

    let mut out = String::new();
    if let Some(title) = &chart.title {
        let _ = writeln!(out, "{}", truncate_display_width(title, max_width));
    }

    let gutter = " ".repeat(label_width + LABEL_GAP);
    let (labels, axis) = draw_axis(chart, low, high, chart_width, &column);
    let _ = writeln!(out, "{gutter}{}", labels.trim_end());
    let _ = writeln!(out, "{gutter}{axis}");

    let mut section = None;
    for (task, placed) in chart.tasks.iter().zip(tasks) {
        if task.section != section {
            section = task.section;
            if let Some(index) = section {
                let _ = writeln!(
                    out,
                    "{}",
                    truncate_display_width(&chart.sections[index], max_width)
                );
            }
        }

        let mut row = vec![' '; chart_width];
        if task.tags.milestone {
            let middle = placed.start + (placed.render_end - placed.start) / 2;
            let cell = (column(middle).round() as usize).min(chart_width - 1);
            row[cell] = MILESTONE;
        } else {
            let first = (column(placed.start).round() as usize).min(chart_width - 1);
            let last = (column(placed.render_end).round() as usize).clamp(first + 1, chart_width);
            row[first..last].fill(bar_char(task.tags));
        }

        let name = truncate_display_width(
            &task.name,
            label_width.saturating_sub(display_width(indent)),
        );
        let padding = label_width - display_width(indent) - display_width(&name) + LABEL_GAP;
        let bar: String = row.into_iter().collect();
        let _ = writeln!(
            out,
            "{indent}{name}{}{}",
            " ".repeat(padding),
            bar.trim_end()
        );
    }

    let legend = legend(chart);
    if !legend.is_empty() {
        out.push('\n');
        let mut line = String::new();
        for entry in legend {
            if !line.is_empty() && display_width(&line) + 2 + display_width(&entry) > max_width {
                let _ = writeln!(out, "{line}");
                line.clear();
            }
            if !line.is_empty() {
                line.push_str("  ");
            }
            line.push_str(&entry);
        }
        let _ = writeln!(out, "{line}");
    }

    Some(out.trim_end().to_string())
}

fn bar_char(tags: Tags) -> char {
    if tags.crit {
        BAR_CRIT
    } else if tags.active {
        BAR_ACTIVE
    } else if tags.done {
        BAR_DONE
    } else {
        BAR
    }
}

fn legend(chart: &Gantt) -> Vec<String> {
    let mut entries = Vec::new();
    let any = |pick: fn(&Tags) -> bool| chart.tasks.iter().any(|task| pick(&task.tags));
    let normal = chart
        .tasks
        .iter()
        .any(|task| !task.tags.milestone && bar_char(task.tags) == BAR);
    if !any(|tags| tags.crit || tags.active || tags.done || tags.milestone) {
        return entries;
    }
    if normal {
        entries.push(format!("{BAR} planned"));
    }
    for (present, symbol, label) in [
        (
            any(|tags| tags.active && !tags.crit && !tags.milestone),
            BAR_ACTIVE,
            "active",
        ),
        (
            any(|tags| tags.done && !tags.active && !tags.crit && !tags.milestone),
            BAR_DONE,
            "done",
        ),
        (
            any(|tags| tags.crit && !tags.milestone),
            BAR_CRIT,
            "critical",
        ),
        (any(|tags| tags.milestone), MILESTONE, "milestone"),
    ] {
        if present {
            entries.push(format!("{symbol} {label}"));
        }
    }
    entries
}

/// Returns the tick label row and the axis rule beneath it.
fn draw_axis(
    chart: &Gantt,
    low: i64,
    high: i64,
    chart_width: usize,
    column: &dyn Fn(i64) -> f64,
) -> (String, String) {
    let label_width = display_width(&format_time(low, &chart.axis_format)).max(1);
    let spacing = (label_width + 2) as f64;
    let span = (high - low) as f64;
    let min_step = span * spacing / chart_width as f64;

    let step = match chart.tick_interval {
        Some(Duration::Millis(ms)) => {
            Duration::Millis(ms * ((min_step / ms as f64).ceil() as i64).max(1))
        }
        Some(Duration::Months(months)) => {
            let approx = months * 30.0 * DAY as f64;
            Duration::Months(months * (min_step / approx).ceil().max(1.0))
        }
        None => nice_step(min_step),
    };

    let mut ticks = Vec::new();
    let mut tick = first_tick(low, step);
    while tick <= high && ticks.len() < chart_width {
        ticks.push(tick);
        tick = step.add_to(tick);
    }

    let mut labels = String::new();
    let mut used = 0usize;
    let mut axis = vec!['─'; chart_width];
    for tick in ticks {
        let cell = column(tick).round() as usize;
        let label = format_time(tick, &chart.axis_format);
        let width = display_width(&label);
        if cell + width > chart_width || (used > 0 && cell < used + 2) {
            continue;
        }
        labels.push_str(&" ".repeat(cell - used));
        labels.push_str(&label);
        used = cell + width;
        axis[cell] = '┬';
    }
    if used == 0 {
        labels = truncate_display_width(&format_time(low, &chart.axis_format), chart_width);
        axis[0] = '┬';
    }
    (labels, axis.into_iter().collect())
}

fn nice_step(min_step: f64) -> Duration {
    const STEPS: [i64; 19] = [
        1,
        10,
        100,
        SECOND,
        5 * SECOND,
        15 * SECOND,
        30 * SECOND,
        MINUTE,
        5 * MINUTE,
        15 * MINUTE,
        30 * MINUTE,
        HOUR,
        3 * HOUR,
        6 * HOUR,
        12 * HOUR,
        DAY,
        2 * DAY,
        WEEK,
        2 * WEEK,
    ];
    if let Some(&step) = STEPS.iter().find(|&&step| step as f64 >= min_step) {
        return Duration::Millis(step);
    }
    let months = [1.0, 3.0, 6.0, 12.0]
        .into_iter()
        .find(|months| months * 30.0 * DAY as f64 >= min_step)
        .unwrap_or_else(|| (min_step / (365.0 * DAY as f64)).ceil() * 12.0);
    Duration::Months(months)
}

fn first_tick(low: i64, step: Duration) -> i64 {
    match step {
        Duration::Millis(ms) if ms % WEEK == 0 => {
            // Weekly ticks land on Mondays; 1970-01-05 was a Monday.
            let monday = 4 * DAY;
            monday
                + (low - monday).div_euclid(ms) * ms
                + if (low - monday) % ms == 0 { 0 } else { ms }
        }
        Duration::Millis(ms) => low.div_euclid(ms) * ms + if low % ms == 0 { 0 } else { ms },
        Duration::Months(_) => {
            let (year, month, day) = civil_from_days(low.div_euclid(DAY));
            let first = days_from_civil(year, month, 1) * DAY;
            if day == 1 && low == first {
                first
            } else {
                add_months(first, 1)
            }
        }
    }
}

fn add_months(time: i64, months: i64) -> i64 {
    let day = time.div_euclid(DAY);
    let time_of_day = time.rem_euclid(DAY);
    let (year, month, day) = civil_from_days(day);
    let index = year * 12 + i64::from(month) - 1 + months;
    let (year, month) = (index.div_euclid(12), (index.rem_euclid(12) + 1) as u32);
    let day = day.min(days_in_month(year, month));
    days_from_civil(year, month, day) * DAY + time_of_day
}

/// Parses `text` with a dayjs-style format such as `YYYY-MM-DD HH:mm`,
/// falling back to ISO dates.
fn parse_date(text: &str, format: &str) -> Option<i64> {
    parse_with_format(text.trim(), format)
        .or_else(|| parse_with_format(text.trim(), "YYYY-MM-DD"))
        .or_else(|| parse_with_format(text.trim(), "YYYY-MM-DDTHH:mm:ss"))
        .or_else(|| parse_with_format(text.trim(), "YYYY-MM-DD HH:mm:ss"))
        .or_else(|| parse_with_format(text.trim(), "YYYY-MM-DD HH:mm"))
}

fn parse_with_format(text: &str, format: &str) -> Option<i64> {
    const TOKENS: [&str; 22] = [
        "YYYY", "MMMM", "MMM", "SSS", "YY", "MM", "DD", "HH", "hh", "mm", "ss", "SS", "Do", "M",
        "D", "H", "h", "m", "s", "S", "X", "x",
    ];

    let (mut year, mut month, mut day) = (1970i64, 1u32, 1u32);
    let (mut hour, mut minute, mut second, mut millis) = (0i64, 0i64, 0i64, 0i64);
    let mut pm = None;
    let mut epoch = None;
    let mut input = text;
    let mut rest = format;

    while !rest.is_empty() {
        if let Some(escaped) = rest.strip_prefix('[') {
            let end = escaped.find(']')?;
            input = input.strip_prefix(&escaped[..end])?;
            rest = &escaped[end + 1..];
            continue;
        }
        if let Some(token) = TOKENS.iter().find(|token| rest.starts_with(**token)) {
            rest = &rest[token.len()..];
            if matches!(*token, "MMMM" | "MMM") {
                let (index, len) = MONTH_NAMES.iter().enumerate().find_map(|(index, name)| {
                    let len = if *token == "MMM" { 3 } else { name.len() };
                    input
                        .get(..len)
                        .filter(|prefix| prefix.eq_ignore_ascii_case(&name[..len]))
                        .map(|_| (index, len))
                })?;
                month = index as u32 + 1;
                input = &input[len..];
                continue;
            }
            let max_digits = match *token {
                "YYYY" => 4,
                "X" | "x" => 16,
                "SSS" => 3,
                "S" => 1,
                _ => 2,
            };
            let negative = matches!(*token, "X" | "x") && input.starts_with('-');
            let digits_start = usize::from(negative);
            let digits = input[digits_start..]
                .bytes()
                .take(max_digits)
                .take_while(u8::is_ascii_digit)
                .count();
            if digits == 0 {
                return None;
            }
            let mut value: i64 = input[digits_start..digits_start + digits].parse().ok()?;
            if negative {
                value = -value;
            }
            input = &input[digits_start + digits..];
            match *token {
                "YYYY" => year = value,
                "YY" => {
                    year = if value < 69 {
                        2000 + value
                    } else {
                        1900 + value
                    }
                }
                "MM" | "M" => month = u32::try_from(value).ok()?,
                "DD" | "D" | "Do" => {
                    day = u32::try_from(value).ok()?;
                    if *token == "Do" {
                        input = input.get(2..)?;
                    }
                }
                "HH" | "H" | "hh" | "h" => hour = value,
                "mm" | "m" => minute = value,
                "ss" | "s" => second = value,
                "SSS" => millis = value,
                "SS" => millis = value * 10,
                "S" => millis = value * 100,
                "X" => epoch = Some(value * SECOND),
                "x" => epoch = Some(value),
                _ => {}
            }
            continue;
        }
        if rest.starts_with('A') || rest.starts_with('a') {
            let marker = input.get(..2)?.to_ascii_lowercase();
            pm = Some(match marker.as_str() {
                "am" => false,
                "pm" => true,
                _ => return None,
            });
            input = &input[2..];
            rest = &rest[1..];
            continue;
        }
        let ch = rest.chars().next()?;
        input = input.strip_prefix(ch)?;
        rest = &rest[ch.len_utf8()..];
    }

    if !input.is_empty() {
        return None;
    }
    if let Some(epoch) = epoch {
        return Some(epoch);
    }
    if !(1..=12).contains(&month) || day == 0 || day > days_in_month(year, month) {
        return None;
    }
    match pm {
        Some(true) if hour < 12 => hour += 12,
        Some(false) if hour == 12 => hour = 0,
        _ => {}
    }
    if hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    Some(
        days_from_civil(year, month, day) * DAY
            + hour * HOUR
            + minute * MINUTE
            + second * SECOND
            + millis,
    )
}

/// Formats `time` with the strftime subset Mermaid's `axisFormat` uses.
fn format_time(time: i64, format: &str) -> String {
    let days = time.div_euclid(DAY);
    let of_day = time.rem_euclid(DAY);
    let (year, month, day) = civil_from_days(days);
    let (hour, minute, second) = (
        of_day / HOUR,
        of_day % HOUR / MINUTE,
        of_day % MINUTE / SECOND,
    );
    let month_name = MONTH_NAMES[month as usize - 1];
    let weekday_name = WEEKDAY_NAMES[weekday(days) as usize];

    let mut out = String::new();
    let mut chars = format.chars();
    while let Some(ch) = chars.next() {
        if ch != '%' {
            out.push(ch);
            continue;
        }
        let Some(spec) = chars.next() else {
            out.push('%');
            break;
        };
        let _ = match spec {
            'Y' => write!(out, "{year}"),
            'y' => write!(out, "{:02}", year.rem_euclid(100)),
            'm' => write!(out, "{month:02}"),
            'd' => write!(out, "{day:02}"),
            'e' => write!(out, "{day:>2}"),
            'H' => write!(out, "{hour:02}"),
            'I' => write!(out, "{:02}", (hour + 11) % 12 + 1),
            'p' => write!(out, "{}", if hour < 12 { "AM" } else { "PM" }),
            'M' => write!(out, "{minute:02}"),
            'S' => write!(out, "{second:02}"),
            'L' => write!(out, "{:03}", of_day % SECOND),
            'b' => write!(out, "{}", &month_name[..3]),
            'B' => write!(out, "{month_name}"),
            'a' => write!(out, "{}", &weekday_name[..3]),
            'A' => write!(out, "{weekday_name}"),
            'j' => write!(out, "{:03}", days - days_from_civil(year, 1, 1) + 1),
            '%' => write!(out, "%"),
            other => write!(out, "%{other}"),
        };
    }
    out
}

/// 0 = Sunday.
fn weekday(days: i64) -> u32 {
    (days + 4).rem_euclid(7) as u32
}

fn is_leap_year(year: i64) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        2 if is_leap_year(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Days since 1970-01-01 (Howard Hinnant's algorithm).
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month = i64::from(month);
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let days = days + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_index + 2) / 5 + 1) as u32;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    } as u32;
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "gantt
    title Project Plan
    dateFormat YYYY-MM-DD
    section Design
    Research      :done, r1, 2024-01-01, 7d
    Mockups       :active, m1, after r1, 8d
    section Build
    Backend       :crit, b1, 2024-01-15, 2024-01-29
    Frontend      :10d
    Release       :milestone, after b1, 0d
";

    fn date(text: &str) -> i64 {
        parse_date(text, "YYYY-MM-DD").unwrap()
    }

    #[test]
    fn civil_round_trips() {
        for days in [-800_000, -1, 0, 59, 11_016, 19_723, 2_000_000] {
            let (year, month, day) = civil_from_days(days);
            assert_eq!(days_from_civil(year, month, day), days);
        }
        assert_eq!(civil_from_days(19_723), (2024, 1, 1));
        assert_eq!(weekday(19_723), 1, "2024-01-01 was a Monday");
    }

    #[test]
    fn parses_dayjs_formats() {
        assert_eq!(date("2024-01-01"), 19_723 * DAY);
        assert_eq!(
            parse_date("01/02/2024 14:30", "DD/MM/YYYY HH:mm"),
            Some(date("2024-02-01") + 14 * HOUR + 30 * MINUTE)
        );
        assert_eq!(parse_date("1704067200", "X"), Some(date("2024-01-01")));
        assert_eq!(
            parse_date("Mar 5 2024", "MMM D YYYY"),
            Some(date("2024-03-05"))
        );
        assert_eq!(parse_date("2024-02-30", "YYYY-MM-DD"), None);
        assert_eq!(parse_date("10d", "YYYY-MM-DD"), None);
    }

    #[test]
    fn formats_axis_labels() {
        let time = date("2024-03-05") + 9 * HOUR + 7 * MINUTE;
        assert_eq!(format_time(time, "%Y-%m-%d"), "2024-03-05");
        assert_eq!(format_time(time, "%b %e, %H:%M"), "Mar  5, 09:07");
        assert_eq!(format_time(time, "%a %I%p %%"), "Tue 09AM %");
    }

    #[test]
    fn resolves_task_placement() {
        let chart = parse(SAMPLE).unwrap();
        let placed = chart.resolve().unwrap();
        let spans: Vec<(i64, i64)> = placed.iter().map(|p| (p.start, p.end)).collect();
        assert_eq!(
            spans,
            [
                (date("2024-01-01"), date("2024-01-08")),
                (date("2024-01-08"), date("2024-01-16")),
                (date("2024-01-15"), date("2024-01-29")),
                (date("2024-01-29"), date("2024-02-08")),
                (date("2024-01-29"), date("2024-01-29")),
            ]
        );
        assert_eq!(chart.tasks[3].section, Some(1));
    }

    #[test]
    fn resolves_forward_references_and_until() {
        let chart = parse(
            "gantt\n  A :a, after b, 2d\n  B :b, 2024-01-01, 1d\n  C :c, 2024-01-01, until a\n",
        )
        .unwrap();
        let placed = chart.resolve().unwrap();
        assert_eq!(placed[0].start, date("2024-01-02"));
        assert_eq!(placed[2].end, date("2024-01-02"));
    }

    #[test]
    fn excluded_weekends_extend_durations() {
        let chart = parse("gantt\n  excludes weekends\n  Work :2024-01-05, 2d\n").unwrap();
        let placed = chart.resolve().unwrap()[0];
        // Friday + 2 working days skips Saturday and Sunday.
        assert_eq!(placed.end, date("2024-01-09"));
        assert_eq!(placed.render_end, date("2024-01-09"));
    }

    #[test]
    fn month_durations_follow_the_calendar() {
        assert_eq!(
            Duration::Months(1.0).add_to(date("2024-01-31")),
            date("2024-02-29")
        );
        assert_eq!(
            parse_duration("1.5w"),
            Some(Duration::Millis(10 * DAY + 12 * HOUR))
        );
    }

    #[test]
    fn unresolvable_charts_are_rejected() {
        assert!(render("gantt\n  A :after missing, 1d\n", 80).is_none());
        assert!(render("gantt\n  title Only a title\n", 80).is_none());
        assert!(render("gantt\n  A :1d\n", 80).is_none());
    }

    #[test]
    fn renders_rows_axis_and_legend() {
        let rendered = render(SAMPLE, 70).unwrap();
        let lines: Vec<&str> = rendered.lines().collect();
        assert_eq!(lines[0], "Project Plan");
        assert!(lines[1].contains("2024-01-"), "{rendered}");
        assert!(
            lines[2].contains('┬') && lines[2].contains('─'),
            "{rendered}"
        );
        assert_eq!(lines[3], "Design");

        let research = lines.iter().find(|line| line.contains("Research")).unwrap();
        let mockups = lines.iter().find(|line| line.contains("Mockups")).unwrap();
        let research_end = research.chars().count() - 1;
        let mockups_start = mockups.chars().position(|ch| ch == BAR_ACTIVE).unwrap();
        assert!(mockups_start > research_end, "{rendered}");

        let release = lines.iter().find(|line| line.contains("Release")).unwrap();
        assert!(release.contains(MILESTONE), "{rendered}");
        assert!(lines.iter().any(|line| line.contains(BAR_CRIT)));
        assert!(
            lines.last().unwrap().contains("▓ active")
                && lines.last().unwrap().contains("◆ milestone"),
            "{rendered}"
        );
        assert!(
            lines.iter().all(|line| display_width(line) <= 70),
            "{rendered}"
        );
    }

    #[test]
    fn narrow_widths_truncate_task_names() {
        let rendered = render(SAMPLE, 24).unwrap();
        assert!(
            rendered.lines().all(|line| display_width(line) <= 24),
            "{rendered}"
        );
        assert!(render(SAMPLE, 12).is_none());
    }
}
