use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::Line,
    widgets::{Block, Borders, Paragraph},
};
use serde_json::Value;
use std::collections::{BTreeMap, VecDeque};

const HISTORY_LIMIT: usize = 60;
const INK: Color = Color::Rgb(239, 237, 231);
const MUTED: Color = Color::Rgb(157, 157, 153);
const CHARCOAL: Color = Color::Rgb(25, 26, 28);

struct Sample {
    at: u64,
    cpu: Option<f64>,
    memory: Option<f64>,
    interfaces: BTreeMap<String, (Option<f64>, Option<f64>)>,
}

#[derive(Default)]
pub(super) struct HostView {
    history: VecDeque<Sample>,
    scroll: usize,
}

impl HostView {
    pub(super) fn record(&mut self, snapshot: &Value) {
        let host = &snapshot["host_metrics"];
        let Some(at) = host["sampled_at"]
            .as_u64()
            .or_else(|| snapshot["timestamp"].as_u64())
        else {
            return;
        };
        let sample = Sample {
            at,
            cpu: percent(&host["cpu"]["percent"]),
            memory: memory_percent(&host["memory"]),
            interfaces: host["network"]["interfaces"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|interface| {
                    Some((
                        interface["name"].as_str()?.to_owned(),
                        (
                            number(&interface["rx_bytes_per_second"]),
                            number(&interface["tx_bytes_per_second"]),
                        ),
                    ))
                })
                .collect(),
        };
        if let Some(existing) = self.history.iter_mut().find(|sample| sample.at == at) {
            *existing = sample;
        } else {
            let index = self
                .history
                .iter()
                .position(|sample| sample.at > at)
                .unwrap_or(self.history.len());
            self.history.insert(index, sample);
            if self.history.len() > HISTORY_LIMIT {
                self.history.pop_front();
            }
        }
    }

    pub(super) fn scroll(&mut self, delta: i16) {
        self.scroll = self.scroll.saturating_add_signed(isize::from(delta));
    }

    pub(super) fn draw(&mut self, frame: &mut Frame, area: Rect, snapshot: &Value) {
        let block = Block::default()
            .borders(Borders::ALL)
            .title(" Host monitor ")
            .border_style(Style::default().fg(MUTED))
            .style(Style::default().fg(INK).bg(CHARCOAL));
        let inner = block.inner(area);
        let lines = self.lines(inner.width as usize, snapshot);
        let max_scroll = lines.len().saturating_sub(inner.height as usize);
        self.scroll = self.scroll.min(max_scroll);
        let end = (self.scroll + inner.height as usize).min(lines.len());
        let block = block.title_bottom(format!(
            " {}-{} / {} · ↑/↓ scroll ",
            if lines.is_empty() { 0 } else { self.scroll + 1 },
            end,
            lines.len()
        ));
        frame.render_widget(block, area);
        frame.render_widget(
            Paragraph::new(
                lines
                    .into_iter()
                    .skip(self.scroll)
                    .take(inner.height as usize)
                    .collect::<Vec<_>>(),
            )
            .style(Style::default().fg(INK).bg(CHARCOAL)),
            inner,
        );
    }

    fn lines(&self, width: usize, snapshot: &Value) -> Vec<Line<'static>> {
        if width == 0 {
            return vec![];
        }
        let host = &snapshot["host_metrics"];
        let mut lines = vec![];
        let label = host["label"].as_str().unwrap_or("Host");
        let os = host["os"].as_str().unwrap_or("OS pending");
        push(
            &mut lines,
            width,
            &format!("Controller host · {label} · {os}"),
            true,
        );
        let at = host["sampled_at"].as_u64().map_or_else(
            || "Collection time pending".into(),
            |at| {
                format!(
                    "Collected {:02}:{:02}:{:02} UTC",
                    at / 3600 % 24,
                    at / 60 % 60,
                    at % 60
                )
            },
        );
        let interval = number(&host["interval_seconds"]).map_or_else(
            || "interval pending".into(),
            |seconds| format!("interval {seconds:.1} s"),
        );
        push(&mut lines, width, &format!("{at} · {interval}"), false);
        if let Some(errors) = host["errors"].as_array() {
            for error in errors.iter().filter_map(Value::as_str) {
                push(
                    &mut lines,
                    width,
                    &format!("Collection error: {error}"),
                    false,
                );
            }
        }
        lines.push(Line::default());
        let cpu = percent(&host["cpu"]["percent"]);
        let count = host["cpu"]["logical_cores"]
            .as_u64()
            .map_or_else(|| "Pending".into(), |count| count.to_string());
        push(
            &mut lines,
            width,
            &format!(
                "CPU total {} {}",
                percentage(cpu),
                meter(cpu, width.saturating_sub(21).min(24))
            ),
            true,
        );
        push(
            &mut lines,
            width,
            &format!("{count} logical CPUs · 100% = all logical CPUs busy"),
            false,
        );
        let memory = &host["memory"];
        push(
            &mut lines,
            width,
            &format!(
                "Memory used {} / {} ({})",
                bytes(&memory["used_bytes"]),
                bytes(&memory["total_bytes"]),
                percentage(memory_percent(memory))
            ),
            true,
        );
        push(
            &mut lines,
            width,
            &format!(
                "Available {} · Cached {}",
                bytes(&memory["available_bytes"]),
                bytes(&memory["cached_bytes"])
            ),
            false,
        );
        push(
            &mut lines,
            width,
            &format!(
                "Swap used {} / {}",
                bytes(&memory["swap_used_bytes"]),
                bytes(&memory["swap_total_bytes"])
            ),
            false,
        );
        for categories in [
            [("Active", "active_bytes"), ("Inactive", "inactive_bytes")],
            [("Wired", "wired_bytes"), ("Compressed", "compressed_bytes")],
        ] {
            let readings = categories
                .iter()
                .filter(|(_, field)| number(&memory[*field]).is_some())
                .map(|(label, field)| format!("{label} {}", bytes(&memory[*field])))
                .collect::<Vec<_>>();
            if !readings.is_empty() {
                push(&mut lines, width, &readings.join(" · "), false);
            }
        }
        lines.push(Line::default());
        push(
            &mut lines,
            width,
            &format!(
                "Recent session · {}/60 samples · · missing",
                self.history.len()
            ),
            true,
        );
        let chart_width = width.saturating_sub(6).max(1);
        let cpu_history: Vec<_> = self.history.iter().map(|sample| sample.cpu).collect();
        let memory_history: Vec<_> = self.history.iter().map(|sample| sample.memory).collect();
        push(
            &mut lines,
            width,
            &format!("CPU   {}", trace(&cpu_history, 100., chart_width)),
            false,
        );
        push(
            &mut lines,
            width,
            &format!("RAM   {}", trace(&memory_history, 100., chart_width)),
            false,
        );
        push(&mut lines, width, "Oldest → newest · ▁ 0% to █ 100%", false);
        push(
            &mut lines,
            width,
            "Pending CPU/network rates need two successful collections.",
            false,
        );
        lines.push(Line::default());
        push(&mut lines, width, "Logical CPUs", true);
        let cores = host["cpu"]["cores"].as_array();
        if let Some(cores) = cores.filter(|cores| !cores.is_empty()) {
            let columns = (width / 32).clamp(1, 3);
            let column_width = width / columns;
            for row in cores.chunks(columns) {
                let text = row
                    .iter()
                    .map(|core| {
                        let id = core["id"]
                            .as_u64()
                            .map_or_else(|| "?".into(), |id| id.to_string());
                        let percent = percent(&core["percent"]);
                        let text = format!(
                            "CPU {id:<3} {:>7} {}",
                            percentage(percent),
                            meter(percent, column_width.saturating_sub(18).min(16))
                        );
                        let padding = column_width.saturating_sub(Line::from(text.clone()).width());
                        format!("{text}{}", " ".repeat(padding))
                    })
                    .collect::<String>();
                push(&mut lines, width, text.trim_end(), false);
            }
        } else {
            push(&mut lines, width, "Core measurements pending.", false);
        }
        lines.push(Line::default());
        push(&mut lines, width, "Network interfaces", true);
        push(
            &mut lines,
            width,
            "Each interface is separate; counters are cumulative.",
            false,
        );
        let interfaces = host["network"]["interfaces"].as_array();
        if let Some(interfaces) = interfaces.filter(|interfaces| !interfaces.is_empty()) {
            for interface in interfaces {
                let name = interface["name"].as_str().unwrap_or("Unnamed interface");
                push(&mut lines, width, name, true);
                for (label, rate, counter) in [
                    ("Rx", "rx_bytes_per_second", "rx_bytes"),
                    ("Tx", "tx_bytes_per_second", "tx_bytes"),
                ] {
                    push(
                        &mut lines,
                        width,
                        &format!(
                            "  {label} {} · counter {}",
                            rate_text(number(&interface[rate])),
                            bytes(&interface[counter])
                        ),
                        false,
                    );
                }
                let rx: Vec<_> = self
                    .history
                    .iter()
                    .map(|sample| sample.interfaces.get(name).and_then(|rates| rates.0))
                    .collect();
                let tx: Vec<_> = self
                    .history
                    .iter()
                    .map(|sample| sample.interfaces.get(name).and_then(|rates| rates.1))
                    .collect();
                let peak = rx.iter().chain(&tx).flatten().copied().reduce(f64::max);
                push(
                    &mut lines,
                    width,
                    &format!("  History peak {}", rate_text(peak)),
                    false,
                );
                push(
                    &mut lines,
                    width,
                    &format!(
                        "  Rx  {}",
                        trace(&rx, peak.unwrap_or(1.).max(1.), chart_width)
                    ),
                    false,
                );
                push(
                    &mut lines,
                    width,
                    &format!(
                        "  Tx  {}",
                        trace(&tx, peak.unwrap_or(1.).max(1.), chart_width)
                    ),
                    false,
                );
                lines.push(Line::default());
            }
        } else {
            push(&mut lines, width, "Interface measurements pending.", false);
        }
        lines.push(Line::default());
        push(&mut lines, width, "Measurement scope", true);
        push(
            &mut lines,
            width,
            "Whole controller host; separate from service and Docker VM usage.",
            false,
        );
        for (label, field) in [("Available", "available_basis"), ("Cache", "cached_basis")] {
            if let Some(basis) = memory[field].as_str() {
                push(&mut lines, width, &format!("{label} basis: {basis}"), false);
            }
        }
        lines
    }
}

fn number(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .filter(|value| value.is_finite() && *value >= 0.)
}

fn percent(value: &Value) -> Option<f64> {
    number(value).filter(|value| *value <= 100.)
}

fn memory_percent(memory: &Value) -> Option<f64> {
    let used = number(&memory["used_bytes"])?;
    let total = number(&memory["total_bytes"])?;
    (total > 0. && used <= total).then_some(used / total * 100.)
}

fn percentage(value: Option<f64>) -> String {
    value.map_or_else(|| "Pending".into(), |value| format!("{value:.1}%"))
}

fn scaled_bytes(mut value: f64) -> String {
    let units = ["B", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];
    let mut unit = 0;
    while value >= 1024. && unit < units.len() - 1 {
        value /= 1024.;
        unit += 1;
    }
    if unit == 0 {
        format!("{value:.0} B")
    } else {
        format!("{value:.1} {}", units[unit])
    }
}

fn bytes(value: &Value) -> String {
    number(value).map_or_else(|| "Pending".into(), scaled_bytes)
}

fn rate_text(value: Option<f64>) -> String {
    value.map_or_else(
        || "Pending".into(),
        |value| format!("{}/s", scaled_bytes(value)),
    )
}

fn meter(value: Option<f64>, width: usize) -> String {
    let filled = value.map(|value| (value / 100. * width as f64).round() as usize);
    format!(
        "[{}]",
        (0..width)
            .map(|column| match filled {
                Some(filled) if column < filled => '━',
                Some(_) => '─',
                None => '·',
            })
            .collect::<String>()
    )
}

fn trace(values: &[Option<f64>], maximum: f64, width: usize) -> String {
    const GLYPHS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    values
        .iter()
        .skip(values.len().saturating_sub(width))
        .map(|value| {
            value.map_or('·', |value| {
                GLYPHS[(value / maximum * 7.).round().clamp(0., 7.) as usize]
            })
        })
        .collect()
}

fn push(lines: &mut Vec<Line<'static>>, width: usize, text: &str, heading: bool) {
    let style = if heading {
        Style::default().fg(INK).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(MUTED)
    };
    let mut line = String::new();
    let mut length = 0;
    for character in text.chars().map(|character| {
        if character.is_control() {
            ' '
        } else {
            character
        }
    }) {
        let cell_width = Line::from(character.to_string()).width();
        if length + cell_width > width && !line.is_empty() {
            lines.push(Line::styled(std::mem::take(&mut line), style));
            length = 0;
        }
        line.push(character);
        length += cell_width;
    }
    if !line.is_empty() {
        lines.push(Line::styled(line, style));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};
    use serde_json::json;

    fn snapshot(at: u64, cpu: Value) -> Value {
        json!({"timestamp":at,"host_metrics":{
            "label":"This host","os":"linux","sampled_at":at,"interval_seconds":5.,
            "cpu":{"logical_cores":2,"percent":cpu,"cores":[{"id":0,"percent":0.},{"id":1,"percent":null}]},
            "memory":{"used_bytes":0,"total_bytes":1024,"available_bytes":1024,"cached_bytes":0,"swap_used_bytes":0,"swap_total_bytes":0},
            "network":{"interfaces":[{"name":"eth0","rx_bytes":1024,"tx_bytes":0,"rx_bytes_per_second":0.,"tx_bytes_per_second":null}]},
            "errors":[]
        }})
    }

    fn render(view: &mut HostView, snapshot: &Value, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| view.draw(frame, frame.area(), snapshot))
            .unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>()
    }

    #[test]
    fn measured_zero_is_visible_while_unknown_rates_and_cores_stay_pending() {
        let mut view = HostView::default();
        let state = snapshot(10, json!(0.));
        view.record(&state);
        let screen = render(&mut view, &state, 100, 55);
        assert!(screen.contains("CPU total 0.0%"));
        assert!(screen.contains("Memory used 0 B / 1.0 KiB (0.0%)"));
        assert!(screen.contains("CPU 0      0.0%"));
        assert!(screen.contains("CPU 1   Pending"));
        assert!(screen.contains("Rx 0 B/s · counter 1.0 KiB"));
        assert!(screen.contains("Tx Pending · counter 0 B"));
        assert!(screen.contains("CPU   ▁"));
    }

    #[test]
    fn history_preserves_gaps_and_retains_only_sixty_unique_session_samples() {
        let mut view = HostView::default();
        view.record(&snapshot(1, json!(0.)));
        view.record(&snapshot(2, Value::Null));
        view.record(&snapshot(3, json!(100.)));
        let screen = render(&mut view, &snapshot(3, json!(100.)), 100, 45);
        assert!(screen.contains("CPU   ▁·█"));
        view.record(&snapshot(3, json!(100.)));
        assert_eq!(view.history.len(), 3);
        for at in 4..=65 {
            view.record(&snapshot(at, json!(50.)));
        }
        assert_eq!(view.history.len(), 60);
        assert_eq!(view.history.front().unwrap().at, 6);
        assert_eq!(view.history.back().unwrap().at, 65);
        let screen = render(&mut view, &snapshot(65, json!(50.)), 100, 45);
        assert!(screen.contains("Recent session · 60/60 samples"));
    }

    #[test]
    fn invalid_numbers_never_become_zero_readings() {
        let mut view = HostView::default();
        let mut state = snapshot(1, json!(-1.));
        state["host_metrics"]["memory"]["used_bytes"] = json!("NaN");
        state["host_metrics"]["network"]["interfaces"][0]["rx_bytes_per_second"] = json!(-5.);
        view.record(&state);
        let screen = render(&mut view, &state, 100, 55);
        assert!(screen.contains("CPU total Pending"));
        assert!(screen.contains("Memory used Pending"));
        assert!(screen.contains("Rx Pending · counter 1.0 KiB"));
        assert!(screen.contains("History peak Pending"));
        assert!(screen.contains("CPU   ·"));
    }

    #[test]
    fn network_rates_and_traces_are_separate_for_each_interface() {
        let mut view = HostView::default();
        let mut state = snapshot(1, json!(0.));
        state["host_metrics"]["network"]["interfaces"] = json!([
            {"name":"eth0","rx_bytes":2048,"tx_bytes":1024,"rx_bytes_per_second":1024.,"tx_bytes_per_second":0.},
            {"name":"lo","rx_bytes":0,"tx_bytes":0,"rx_bytes_per_second":0.,"tx_bytes_per_second":0.}
        ]);
        view.record(&state);
        let screen = render(&mut view, &state, 100, 60);
        assert!(screen.contains("Rx 1.0 KiB/s · counter 2.0 KiB"));
        assert!(screen.contains("History peak 1.0 KiB/s"));
        assert!(screen.contains("History peak 0 B/s"));
        assert!(screen.contains("Rx  █"));
        assert!(screen.contains("Rx  ▁"));
    }

    #[test]
    fn compact_terminal_can_scroll_to_every_core_interface_and_collection_error() {
        let mut view = HostView::default();
        let mut state = snapshot(1, Value::Null);
        state["host_metrics"]["cpu"]["cores"] = json!(
            (0..64)
                .map(|id| json!({"id":id,"percent":0.}))
                .collect::<Vec<_>>()
        );
        state["host_metrics"]["errors"] = json!(["CPU counters reset; waiting for a new interval"]);
        let interfaces = state["host_metrics"]["network"]["interfaces"]
            .as_array_mut()
            .unwrap();
        for id in 0..20 {
            interfaces.push(json!({"name":format!("test{id}"),"rx_bytes":0,"tx_bytes":0}));
        }
        view.record(&state);
        let start = render(&mut view, &state, 40, 12);
        assert!(start.contains("Controller host"));
        assert!(start.contains("Collection error:"));
        let mut visited = String::new();
        for _ in 0..150 {
            visited.push_str(&render(&mut view, &state, 40, 12));
            view.scroll(5);
        }
        assert!(visited.contains("CPU 63"));
        assert!(visited.contains("test19"));
        view.scroll(i16::MIN);
        assert!(render(&mut view, &state, 40, 12).contains("Controller host"));
        for (width, height) in [(1, 1), (12, 4), (24, 8)] {
            render(&mut view, &state, width, height);
        }
    }
}
