use std::{
    collections::BTreeMap,
    fmt,
    fs::{File, OpenOptions},
    io::{self, Write},
    path::Path,
    sync::{Mutex, MutexGuard, OnceLock},
};

use tracing::{Event, Level, Subscriber, field::Visit};
use tracing_subscriber::{
    EnvFilter,
    fmt::{FmtContext, FormatEvent, FormatFields, format::Writer},
    layer::SubscriberExt,
    registry::LookupSpan,
    util::SubscriberInitExt,
};

const DEFAULT_LOG_FILE: &str = "logs/rust-app.log";
static LOG_FILE: OnceLock<Option<Mutex<File>>> = OnceLock::new();

const LEVEL_WIDTH: usize = 8;
const SCOPE_WIDTH: usize = 13;
const METHOD_WIDTH: usize = SCOPE_WIDTH + 2;
const ACTION_WIDTH: usize = 42;
const STATUS_WIDTH: usize = 6;
const DURATION_WIDTH: usize = 8;
const DETAIL_WIDTH: usize = 72;
const DETAIL_VALUE_WIDTH: usize = 16;

const GREEN: &str = "\x1b[32m";
const ORANGE: &str = "\x1b[38;5;208m";
const RED: &str = "\x1b[31m";
const RESET: &str = "\x1b[0m";

pub fn init_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "rust_app=info,app=info,sqlx::query=debug".into());
    let console = tracing_subscriber::fmt::layer()
        .event_format(LogFormatter {
            ansi: true,
            compact: true,
        })
        .with_ansi(false)
        .with_writer(io::stdout);
    let file = log_file().map(|_| {
        tracing_subscriber::fmt::layer()
            .event_format(LogFormatter {
                ansi: false,
                compact: false,
            })
            .with_ansi(false)
            .with_writer(log_file_writer)
    });

    tracing_subscriber::registry()
        .with(filter)
        .with(console)
        .with(file)
        .init();
}

#[allow(dead_code)]
pub fn write_line(line: &str) {
    let _ = writeln!(io::stdout(), "{}", single_line(line));
}

fn log_file() -> Option<&'static Mutex<File>> {
    LOG_FILE
        .get_or_init(|| {
            let path = std::env::var("LOG_FILE").unwrap_or_else(|_| DEFAULT_LOG_FILE.to_owned());
            match open_log_file(&path) {
                Ok(file) => Some(Mutex::new(file)),
                Err(error) => {
                    eprintln!("Failed to open log file {path}: {error}");
                    None
                }
            }
        })
        .as_ref()
}

fn open_log_file(path: &str) -> io::Result<File> {
    if let Some(parent) = Path::new(path).parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    OpenOptions::new().create(true).append(true).open(path)
}

fn log_file_writer() -> LogFileWriter {
    LogFileWriter {
        file: log_file().and_then(|file| file.lock().ok()),
    }
}

struct LogFileWriter {
    file: Option<MutexGuard<'static, File>>,
}

impl Write for LogFileWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if let Some(file) = self.file.as_mut() {
            file.write_all(buffer)?;
        }
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if let Some(file) = self.file.as_mut() {
            file.flush()?;
        }
        Ok(())
    }
}

struct LogFormatter {
    ansi: bool,
    compact: bool,
}

impl<S, N> FormatEvent<S, N> for LogFormatter
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
    N: for<'writer> FormatFields<'writer> + 'static,
{
    fn format_event(
        &self,
        _context: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result {
        let mut fields = EventFields::default();
        event.record(&mut fields);

        let timestamp = shanghai_timestamp();
        let sql_event = is_sql_event(event, &fields);
        let level = if sql_event {
            &Level::INFO
        } else {
            event.metadata().level()
        };
        let level_text = level_text(level);
        write!(
            writer,
            "{timestamp}  {} ",
            colored(
                self.ansi,
                level_color(level),
                &pad_right(level_text, LEVEL_WIDTH)
            )
        )?;

        if fields.get("http").is_some() {
            format_http_event(&mut writer, level, &fields, self.ansi, self.compact)?;
        } else if sql_event {
            format_sql_event(&mut writer, &fields, self.ansi, self.compact)?;
        } else {
            format_standard_event(
                &mut writer,
                event.metadata().target(),
                &fields,
                self.compact,
            )?;
        }

        writeln!(writer)
    }
}

#[derive(Default)]
struct EventFields {
    values: BTreeMap<String, String>,
}

impl EventFields {
    fn get(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(String::as_str)
    }

    fn first(&self, keys: &[&str]) -> Option<&str> {
        keys.iter().find_map(|key| self.get(key))
    }
}

impl Visit for EventFields {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.values
            .insert(field.name().to_owned(), single_line(value));
    }

    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        self.values
            .insert(field.name().to_owned(), value.to_string());
    }

    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        self.values
            .insert(field.name().to_owned(), value.to_string());
    }

    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        self.values
            .insert(field.name().to_owned(), value.to_string());
    }

    fn record_f64(&mut self, field: &tracing::field::Field, value: f64) {
        self.values
            .insert(field.name().to_owned(), value.to_string());
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn fmt::Debug) {
        self.values.insert(
            field.name().to_owned(),
            single_line(&format!("{value:?}"))
                .trim_matches('"')
                .to_owned(),
        );
    }
}

fn format_http_event(
    writer: &mut Writer<'_>,
    level: &Level,
    fields: &EventFields,
    ansi: bool,
    compact: bool,
) -> fmt::Result {
    let method = abbreviate(fields.get("method").unwrap_or("-"), METHOD_WIDTH);
    let path = if compact {
        abbreviate_path(
            fields.first(&["terminal_path", "path"]).unwrap_or("-"),
            ACTION_WIDTH,
        )
    } else {
        single_line(fields.get("path").unwrap_or("-"))
    };
    let status = fields
        .get("status")
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or_default();
    let duration_ms = duration_ms(fields);
    let detail = http_detail(fields, compact);

    write!(
        writer,
        "{} {} {} {}",
        pad_right(&method, METHOD_WIDTH),
        pad_right_mode(&path, ACTION_WIDTH, compact),
        colored(
            ansi,
            status_color(status),
            &pad_left(&status.to_string(), STATUS_WIDTH)
        ),
        colored(
            ansi,
            duration_color(duration_ms),
            &duration_cell(duration_ms)
        )
    )?;
    if !detail.is_empty() {
        let detail = if compact {
            abbreviate_preserving_spacing(&detail, DETAIL_WIDTH)
        } else {
            detail
        };
        write!(writer, "  {detail}")?;
    } else if *level == Level::ERROR {
        write!(writer, "  {}", abbreviate("Request failed", DETAIL_WIDTH))?;
    }
    Ok(())
}

fn format_sql_event(
    writer: &mut Writer<'_>,
    fields: &EventFields,
    ansi: bool,
    compact: bool,
) -> fmt::Result {
    let statement = if compact {
        fields
            .first(&["summary", "db.statement", "statement", "query"])
            .unwrap_or("SQL")
    } else {
        fields
            .first(&["db.statement", "statement", "query", "summary"])
            .unwrap_or("SQL")
    };
    let action = if compact {
        abbreviate_sql(statement, ACTION_WIDTH)
    } else {
        compact_sql(statement)
    };
    let duration_ms = duration_ms(fields);
    let rows = fields
        .first(&["rows_affected", "rows_returned"])
        .map(|rows| {
            if compact {
                format!("rows={}", abbreviate(rows, 12))
            } else {
                format!("rows={}", single_line(rows))
            }
        })
        .unwrap_or_default();

    write!(
        writer,
        "{} > {} {} {}",
        pad_right("app::sql", SCOPE_WIDTH),
        pad_right_mode(&action, ACTION_WIDTH, compact),
        colored(ansi, GREEN, &pad_left("OK", STATUS_WIDTH)),
        colored(
            ansi,
            duration_color(duration_ms),
            &duration_cell(duration_ms)
        )
    )?;
    if !rows.is_empty() {
        write!(writer, "  {rows}")?;
    }
    Ok(())
}

fn format_standard_event(
    writer: &mut Writer<'_>,
    target: &str,
    fields: &EventFields,
    compact: bool,
) -> fmt::Result {
    let target = normalized_target(target);
    let message = fields.get("message").unwrap_or("Event");
    let details = fields
        .values
        .iter()
        .filter(|(key, _)| key.as_str() != "message")
        .map(|(key, value)| {
            let value = if compact {
                abbreviate_value(key, value, 24)
            } else {
                single_line(value)
            };
            format!("{key}={value}")
        })
        .collect::<Vec<_>>()
        .join("  ");
    let body = if details.is_empty() {
        if compact {
            abbreviate(message, DETAIL_WIDTH)
        } else {
            single_line(message)
        }
    } else if compact {
        abbreviate(&format!("{message}  {details}"), DETAIL_WIDTH)
    } else {
        format!("{}  {details}", single_line(message))
    };

    write!(
        writer,
        "{} > {body}",
        pad_right_mode(&target, SCOPE_WIDTH, compact)
    )
}

fn http_detail(fields: &EventFields, compact: bool) -> String {
    let mut details = Vec::new();
    if let Some(user_id) = fields
        .first(&["user_id", "user"])
        .filter(|user_id| !user_id.is_empty())
    {
        let user_id = if compact {
            short_identifier(user_id)
        } else {
            single_line(user_id)
        };
        details.push(format_detail_field(
            "user_id",
            &user_id,
            DETAIL_VALUE_WIDTH,
            compact,
        ));
    }
    if !compact {
        for key in ["ip", "request_id"] {
            if let Some(value) = fields.get(key).filter(|value| !value.is_empty()) {
                details.push(format_detail_field(key, value, 0, false));
            }
        }
    }
    if let Some(message) = fields.get("message")
        && !message.is_empty()
        && message != "HTTP request"
    {
        details.push(format_detail_field("message", message, 28, compact));
    }
    if let Some(error) = fields.get("error") {
        details.push(format_detail_field("error", error, 28, compact));
    }
    if !compact {
        for (key, value) in &fields.values {
            if matches!(
                key.as_str(),
                "http"
                    | "method"
                    | "path"
                    | "terminal_path"
                    | "status"
                    | "duration_ms"
                    | "elapsed_milliseconds"
                    | "latency_ms"
                    | "user_id"
                    | "user"
                    | "ip"
                    | "request_id"
                    | "message"
                    | "error"
            ) || value.is_empty()
            {
                continue;
            }
            details.push(format_detail_field(key, value, 0, false));
        }
    }
    details.join("  ").trim_end().to_owned()
}

fn format_detail_field(key: &str, value: &str, width: usize, compact: bool) -> String {
    format!("{key}={}", pad_right_mode(value, width, compact))
}

fn duration_ms(fields: &EventFields) -> u64 {
    if let Some(milliseconds) = fields
        .first(&["duration_ms", "elapsed_milliseconds", "latency_ms"])
        .and_then(parse_number)
    {
        return milliseconds.round().max(0.0) as u64;
    }
    if let Some(seconds) = fields
        .first(&["elapsed_secs", "elapsed_seconds"])
        .and_then(parse_number)
    {
        return (seconds * 1000.0).round().max(0.0) as u64;
    }
    fields
        .get("elapsed")
        .and_then(parse_elapsed)
        .unwrap_or_default()
}

fn parse_number(value: &str) -> Option<f64> {
    value.trim_matches('"').parse::<f64>().ok()
}

fn parse_elapsed(value: &str) -> Option<u64> {
    let value = value.trim().trim_matches('"');
    let (number, multiplier) = if let Some(number) = value.strip_suffix("ms") {
        (number, 1.0)
    } else if let Some(number) = value.strip_suffix("µs") {
        (number, 0.001)
    } else if let Some(number) = value.strip_suffix("us") {
        (number, 0.001)
    } else if let Some(number) = value.strip_suffix('s') {
        (number, 1000.0)
    } else {
        (value, 1.0)
    };
    parse_number(number).map(|number| (number * multiplier).round().max(0.0) as u64)
}

fn duration_cell(duration_ms: u64) -> String {
    pad_left(&format!("{duration_ms} ms"), DURATION_WIDTH)
}

fn shanghai_timestamp() -> String {
    let timezone = chrono::FixedOffset::east_opt(8 * 60 * 60)
        .expect("UTC+8 must be a valid fixed timezone offset");
    chrono::Utc::now()
        .with_timezone(&timezone)
        .format("%Y-%m-%d %H:%M:%S")
        .to_string()
}

fn is_sql_event(event: &Event<'_>, fields: &EventFields) -> bool {
    event.metadata().target().starts_with("sqlx")
        || fields.get("db.statement").is_some()
        || fields.get("statement").is_some()
}

fn level_text(level: &Level) -> &'static str {
    match *level {
        Level::ERROR => "Error",
        Level::WARN => "Warn",
        Level::INFO => "INFO",
        Level::DEBUG => "DEBUG",
        Level::TRACE => "TRACE",
    }
}

fn level_color(level: &Level) -> &'static str {
    match *level {
        Level::ERROR => RED,
        Level::WARN => ORANGE,
        Level::INFO => GREEN,
        Level::DEBUG | Level::TRACE => "",
    }
}

fn status_color(status: u16) -> &'static str {
    match status {
        200..=399 => GREEN,
        400..=499 => RED,
        500..=599 => ORANGE,
        _ => "",
    }
}

fn duration_color(duration_ms: u64) -> &'static str {
    if duration_ms > 1000 {
        RED
    } else if duration_ms > 100 {
        ORANGE
    } else {
        ""
    }
}

fn colored(ansi: bool, color: &str, value: &str) -> String {
    if !ansi || color.is_empty() {
        value.to_owned()
    } else {
        format!("{color}{value}{RESET}")
    }
}

fn normalized_target(target: &str) -> String {
    if target == "rust_app" {
        return "app::server".to_owned();
    }
    target
        .strip_prefix("rust_app::")
        .map(|target| format!("app::{target}"))
        .unwrap_or_else(|| target.to_owned())
}

fn abbreviate_path(path: &str, width: usize) -> String {
    let sanitized = single_line(path);
    let shortened = sanitized
        .split('/')
        .map(|part| {
            if uuid::Uuid::parse_str(part).is_ok() {
                short_identifier(part)
            } else {
                part.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("/");
    abbreviate_end_with_marker(&shortened, width, '*')
}

fn abbreviate_sql(statement: &str, width: usize) -> String {
    abbreviate(&compact_sql(statement), width)
}

fn compact_sql(statement: &str) -> String {
    single_line(statement)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn abbreviate_value(key: &str, value: &str, width: usize) -> String {
    if key.contains("user_id")
        || key == "user"
        || key.ends_with("_id")
        || uuid::Uuid::parse_str(value).is_ok()
    {
        abbreviate(&short_identifier(value), width)
    } else if key.contains("path") || value.starts_with('/') {
        abbreviate_path(value, width)
    } else if key.contains("sql") || key == "statement" || key == "query" {
        abbreviate_sql(value, width)
    } else {
        abbreviate(value, width)
    }
}

fn short_identifier(value: &str) -> String {
    let value = single_line(value);
    if value.chars().count() <= 12 {
        value
    } else {
        let prefix = value.chars().take(8).collect::<String>();
        format!("{prefix}-***")
    }
}

fn pad_right(value: &str, width: usize) -> String {
    let value = abbreviate(value, width);
    let padding = width.saturating_sub(value.chars().count());
    format!("{value}{}", " ".repeat(padding))
}

fn pad_right_mode(value: &str, width: usize, compact: bool) -> String {
    if compact {
        pad_right(value, width)
    } else {
        let value = single_line(value);
        let padding = width.saturating_sub(value.chars().count());
        format!("{value}{}", " ".repeat(padding))
    }
}

fn pad_left(value: &str, width: usize) -> String {
    let value = abbreviate_left(value, width);
    let padding = width.saturating_sub(value.chars().count());
    format!("{}{value}", " ".repeat(padding))
}

fn abbreviate(value: &str, width: usize) -> String {
    let value = single_line(value);
    if value.chars().count() <= width {
        return value;
    }
    if width <= 1 {
        return "…".chars().take(width).collect();
    }
    format!("{}…", value.chars().take(width - 1).collect::<String>())
}

fn abbreviate_preserving_spacing(value: &str, width: usize) -> String {
    if value.chars().count() <= width {
        return value.to_owned();
    }
    if width <= 1 {
        return "…".chars().take(width).collect();
    }
    format!("{}…", value.chars().take(width - 1).collect::<String>())
}

fn abbreviate_end_with_marker(value: &str, width: usize, marker: char) -> String {
    if value.chars().count() <= width {
        return value.to_owned();
    }
    if width <= 1 {
        return marker.to_string().chars().take(width).collect();
    }
    format!(
        "{}{marker}",
        value.chars().take(width - 1).collect::<String>()
    )
}

fn abbreviate_left(value: &str, width: usize) -> String {
    let value = single_line(value);
    if value.chars().count() <= width {
        return value;
    }
    if width <= 1 {
        return "…".chars().take(width).collect();
    }
    let suffix = value
        .chars()
        .rev()
        .take(width - 1)
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>();
    format!("…{suffix}")
}

#[allow(dead_code)]
fn abbreviate_middle(value: &str, width: usize) -> String {
    abbreviate_middle_with_marker(value, width, '…')
}

fn abbreviate_middle_with_marker(value: &str, width: usize, marker: char) -> String {
    if value.chars().count() <= width {
        return value.to_owned();
    }
    if width <= 1 {
        return marker.to_string().chars().take(width).collect();
    }
    let left = (width - 1) / 2;
    let right = width - 1 - left;
    let prefix = value.chars().take(left).collect::<String>();
    let suffix = value
        .chars()
        .rev()
        .take(right)
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>();
    format!("{prefix}{marker}{suffix}")
}

fn single_line(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character == '\r' || character == '\n' || character == '\t' {
                ' '
            } else if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::{
        ACTION_WIDTH, EventFields, METHOD_WIDTH, ORANGE, RED, SCOPE_WIDTH, abbreviate,
        abbreviate_middle, abbreviate_path, abbreviate_sql, compact_sql, duration_cell,
        duration_color, format_detail_field, http_detail, open_log_file, pad_left, pad_right,
        pad_right_mode, short_identifier, single_line, status_color,
    };
    use std::io::Write;

    #[test]
    fn creates_parent_directory_and_appends_log_lines() {
        let path = "target/test-logs/config-logger.log";
        let marker = format!("logger-test-{}", uuid::Uuid::new_v4());
        let mut file = open_log_file(path).unwrap();
        writeln!(file, "{marker}").unwrap();
        file.flush().unwrap();

        let contents = std::fs::read_to_string(path).unwrap();
        assert!(contents.contains(&marker));
    }

    #[test]
    fn keeps_fields_on_one_bounded_line() {
        assert_eq!(single_line("first\nsecond\tthird"), "first second third");
        assert_eq!(abbreviate("abcdefgh", 6), "abcde…");
        assert_eq!(abbreviate_middle("/very/long/path", 10), "/ver…/path");
    }

    #[test]
    fn abbreviates_paths_ids_and_sql() {
        assert_eq!(
            abbreviate_path(
                "/api/music/5ec5d7bd-196a-43cf-b58f-d17e976bade5/favorite",
                34
            ),
            "/api/music/5ec5d7bd-***/favorite"
        );
        assert_eq!(
            short_identifier("5ec5d7bd-196a-43cf-b58f-d17e976bade5"),
            "5ec5d7bd-***"
        );
        assert_eq!(
            abbreviate_path(
                "/api/this/is/a/very/long/path/that/must/not/shift/status",
                42
            ),
            "/api/this/is/a/very/long/path/that/must/n*"
        );
        assert_eq!(
            abbreviate_sql("SELECT  *\nFROM music WHERE user_id = $1", 24),
            "SELECT * FROM music WHE…"
        );
    }

    #[test]
    fn applies_status_and_duration_threshold_colors() {
        assert_eq!(status_color(200), "\x1b[32m");
        assert_eq!(status_color(401), RED);
        assert_eq!(status_color(500), ORANGE);
        assert_eq!(duration_color(100), "");
        assert_eq!(duration_color(101), ORANGE);
        assert_eq!(duration_color(1001), RED);
    }

    #[test]
    fn right_aligns_status_and_duration_columns() {
        assert_eq!(ACTION_WIDTH, 42);
        assert_eq!(
            format!("{} ", pad_right("GET", METHOD_WIDTH))
                .chars()
                .count(),
            format!("{} > ", pad_right("app::sql", SCOPE_WIDTH))
                .chars()
                .count()
        );
        assert_eq!(
            pad_right("SELECT current_database()", ACTION_WIDTH)
                .chars()
                .count(),
            42
        );
        assert_eq!(pad_right("/api/music/ws", ACTION_WIDTH).chars().count(), 42);
        assert_eq!(pad_left("OK", 6), "    OK");
        assert_eq!(pad_left("101", 6), "   101");
        assert_eq!(pad_left("200", 6), "   200");
        assert_eq!(duration_cell(0), "    0 ms");
        assert_eq!(duration_cell(91), "   91 ms");
        assert_eq!(duration_cell(3284), " 3284 ms");
        assert_eq!(duration_cell(99999), "99999 ms");
    }

    #[test]
    fn left_aligns_and_abbreviates_detail_values() {
        assert_eq!(
            format_detail_field("user_id", "9b9fd548-***", 16, true),
            "user_id=9b9fd548-***    "
        );
        assert_eq!(
            format_detail_field("error", "database connection timeout", 16, true),
            "error=database connec…"
        );

        let mut fields = EventFields::default();
        fields.values.insert(
            "user_id".to_owned(),
            "9b9fd548-9abc-4def-8123-123456789abc".to_owned(),
        );
        fields
            .values
            .insert("message".to_owned(), "Upload success".to_owned());
        assert_eq!(
            http_detail(&fields, true),
            "user_id=9b9fd548-***      message=Upload success"
        );
    }

    #[test]
    fn preserves_complete_values_for_file_logs() {
        let user_id = "9b9fd548-9abc-4def-8123-123456789abc";
        let request_id = "f9dfaf7b-6afb-48df-bec7-f85b38ea3918";
        let path = "/api/music/5ec5d7bd-196a-43cf-b58f-d17e976bade5/a/very/long/resource/path";
        let sql = "SELECT id, title, artist, album FROM music WHERE user_id = $1 ORDER BY created_at DESC";

        assert_eq!(pad_right_mode(path, ACTION_WIDTH, false), path);
        assert_eq!(compact_sql(sql), sql);
        assert_eq!(
            format_detail_field("user_id", user_id, 16, false),
            format!("user_id={user_id}")
        );

        let mut fields = EventFields::default();
        fields
            .values
            .insert("user_id".to_owned(), user_id.to_owned());
        fields
            .values
            .insert("ip".to_owned(), "203.0.113.10".to_owned());
        fields
            .values
            .insert("request_id".to_owned(), request_id.to_owned());
        fields.values.insert(
            "message".to_owned(),
            "Upload success with a complete unabridged message".to_owned(),
        );

        let detail = http_detail(&fields, false);
        assert!(detail.contains(user_id));
        assert!(detail.contains(request_id));
        assert!(detail.contains("Upload success with a complete unabridged message"));
        assert!(!detail.contains('…'));
        assert!(!detail.contains("***"));
    }
}
