use eframe::egui;
use std::{cell::{Cell, RefCell}, ops::RangeInclusive, sync::atomic::{AtomicU64, Ordering}};

enum Content<T> { Value(T), Text(String) }
enum Change { Text(String), Number(f64) }

pub(crate) fn new_input_view() -> egui::Id {
    static NEXT_VIEW: AtomicU64 = AtomicU64::new(1);
    let id = NEXT_VIEW.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
        .expect("Forms numeric view identity exhausted");
    egui::Id::new(("editor-number", id))
}

pub(crate) struct NumberInput<T> {
    content: Content<T>,
    seed: f64,
    label: &'static str,
    speed: f64,
    range: RangeInclusive<f64>,
    max_decimals: usize,
    view_id: egui::Id,
}

fn finite(raw: &str) -> Option<f64> {
    raw.chars().filter(|c| !c.is_whitespace()).map(|c| if c == '−' { '-' } else { c })
        .collect::<String>().parse::<f64>().ok().filter(|value| value.is_finite())
}

impl<T: egui::emath::Numeric + ToString> NumberInput<T> {
    pub(crate) fn new(value: T, label: &'static str, speed: f64, range: RangeInclusive<f64>, max_decimals: usize) -> Self {
        Self { content: Content::Value(value), seed: value.to_f64(), label, speed, range, max_decimals,
            view_id: new_input_view() }
    }

    pub(crate) fn value(&self) -> Result<T, String> {
        match &self.content {
            Content::Value(value) => Ok(*value),
            Content::Text(raw) => {
                let value = finite(raw).ok_or_else(|| format!("{} must be a finite number", self.label))?;
                let (min, max) = (*self.range.start(), *self.range.end());
                let value = if value.total_cmp(&min).is_le() { min }
                    else if value.total_cmp(&max).is_ge() { max } else { value };
                Ok(T::from_f64(value))
            }
        }
    }

    fn number(&mut self, number: f64) {
        if number.is_finite() {
            let value = T::from_f64(number);
            self.seed = value.to_f64();
            self.content = Content::Value(value);
        } else {
            self.content = Content::Text(number.to_string());
        }
    }

    fn text(&mut self, raw: String) {
        self.content = Content::Text(raw);
        if let Ok(value) = self.value() { self.seed = value.to_f64(); }
    }

    fn format_number(value: T, formatter: &egui::style::NumberFormatter, decimals: RangeInclusive<usize>) -> String {
        if T::INTEGRAL { value.to_string() } else { formatter.format(value.to_f64(), decimals) }
    }

    fn format(&self, formatter: &egui::style::NumberFormatter, decimals: RangeInclusive<usize>) -> String {
        match &self.content {
            Content::Value(value) => Self::format_number(*value, formatter, decimals),
            Content::Text(raw) => raw.clone(),
        }
    }

    pub(crate) fn show(&mut self, ui: &mut egui::Ui) -> egui::Response {
        let (label, speed, range, max_decimals, view_id) =
            (self.label, self.speed, self.range.clone(), self.max_decimals, self.view_id);
        let formatter = ui.style().number_formatter.clone();
        // A stable bridge keeps egui's blur parser echo from becoming a text-change signal.
        let seed = self.seed;
        let field = RefCell::new(self);
        let changes = RefCell::new(Vec::new());
        let parser_set = Cell::new(false);
        let mut response = ui.scope_builder(egui::UiBuilder::new().id(view_id), |ui| ui.horizontal(|ui| {
            let caption = ui.label(label);
            ui.add(egui::DragValue::from_get_set(|number| {
                if let Some(number) = number {
                    if !parser_set.replace(false) { changes.borrow_mut().push(Change::Number(number)); }
                }
                seed
            }).custom_parser(|raw| {
                let parsed = finite(raw);
                changes.borrow_mut().push(Change::Text(raw.to_owned()));
                parser_set.set(parsed.is_some());
                parsed
            }).custom_formatter(|_, decimals| {
                match changes.borrow().last() {
                    Some(Change::Number(number)) if number.is_finite() =>
                        Self::format_number(T::from_f64(*number), &formatter, decimals),
                    Some(Change::Number(number)) => number.to_string(),
                    _ => field.borrow().format(&formatter, decimals),
                }
            }).speed(speed).range(range).max_decimals(max_decimals).clamp_existing_to_range(false))
                .labelled_by(caption.id)
        }).inner).inner;
        let text_changed = response.changed();
        for change in changes.into_inner() {
            match change {
                Change::Text(raw) if text_changed => field.borrow_mut().text(raw),
                Change::Number(number) => { field.borrow_mut().number(number); response.mark_changed(); }
                Change::Text(_) => {}
            }
        }
        response
    }
}
