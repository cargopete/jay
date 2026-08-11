//! The overlay: a transparent, always-on-top panel showing the live transcript.
//!
//! Deliberately not hidden from screen capture. The Windows and macOS flags
//! that do that are what the interview-cheating tools use, the macOS one
//! stopped working for ScreenCaptureKit in 15.4 anyway, and jay is for things
//! you can say out loud that you are doing.

use std::collections::VecDeque;
use std::time::Instant;

use crossbeam_channel::Receiver;
use eframe::egui;

/// How many lines the panel keeps. Older ones scroll off and are forgotten by
/// the UI; the transcript itself is not the UI's job to store.
const MAX_LINES: usize = 200;

/// What sort of line this is, which decides how it is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Something somebody said.
    Transcript,
    /// Something jay is offering. Drawn distinctly, because you should never
    /// have to wonder whether you are reading a person or a machine.
    Suggestion,
    /// Housekeeping: budget spent, a gate declining, an error.
    Notice,
}

/// One line in the panel.
#[derive(Debug, Clone)]
pub struct Line {
    /// Who said it: "you", "them", or "jay".
    pub speaker: String,
    pub text: String,
    /// How far behind live this line landed, for the status row.
    pub lag: std::time::Duration,
    pub kind: Kind,
}

impl Line {
    pub fn transcript(speaker: impl Into<String>, text: impl Into<String>, lag: std::time::Duration) -> Self {
        Self { speaker: speaker.into(), text: text.into(), lag, kind: Kind::Transcript }
    }

    pub fn suggestion(text: impl Into<String>, lag: std::time::Duration) -> Self {
        Self { speaker: "jay".into(), text: text.into(), lag, kind: Kind::Suggestion }
    }

    pub fn notice(text: impl Into<String>) -> Self {
        Self {
            speaker: "jay".into(),
            text: text.into(),
            lag: std::time::Duration::ZERO,
            kind: Kind::Notice,
        }
    }
}

/// Run the overlay. Blocks until the window is closed.
pub fn run(rx: Receiver<Line>, model_name: String) -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("jay")
            .with_inner_size([440.0, 320.0])
            .with_min_inner_size([280.0, 140.0])
            .with_transparent(true)
            .with_decorations(false)
            .with_always_on_top(),
        ..Default::default()
    };

    eframe::run_native(
        "jay",
        options,
        Box::new(move |cc| Ok(Box::new(Overlay::new(cc, rx, model_name)))),
    )
}

struct Overlay {
    rx: Receiver<Line>,
    lines: VecDeque<Line>,
    model_name: String,
    started: Instant,
}

impl Overlay {
    fn new(cc: &eframe::CreationContext<'_>, rx: Receiver<Line>, model_name: String) -> Self {
        // Dark, translucent, and low contrast enough to sit over a terminal
        // without becoming the thing you look at.
        let mut visuals = egui::Visuals::dark();
        visuals.panel_fill = egui::Color32::from_rgba_unmultiplied(16, 18, 22, 220);
        visuals.window_fill = visuals.panel_fill;
        cc.egui_ctx.set_visuals(visuals);

        Self {
            rx,
            lines: VecDeque::with_capacity(MAX_LINES),
            model_name,
            started: Instant::now(),
        }
    }
}

impl eframe::App for Overlay {
    /// Transparent so the compositor blends the panel with whatever is behind.
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        for line in self.rx.try_iter() {
            if self.lines.len() == MAX_LINES {
                self.lines.pop_front();
            }
            self.lines.push_back(line);
        }

        {
            // The header doubles as a drag handle: there is no title bar, and
            // a window you cannot move is a window in the way.
            let header = ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("jay")
                        .strong()
                        .color(egui::Color32::from_rgb(150, 190, 255)),
                );
                ui.label(
                    egui::RichText::new(format!("· {}", self.model_name))
                        .small()
                        .weak(),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("×").clicked() {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                    ui.label(
                        egui::RichText::new(format!("{:.0}s", self.started.elapsed().as_secs_f32()))
                            .small()
                            .weak(),
                    );
                });
            });
            if header.response.interact(egui::Sense::drag()).dragged() {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
            }

            ui.separator();

            egui::ScrollArea::vertical()
                .stick_to_bottom(true)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    if self.lines.is_empty() {
                        ui.label(egui::RichText::new("listening…").weak().italics());
                    }
                    for line in &self.lines {
                        match line.kind {
                            Kind::Notice => {
                                ui.label(
                                    egui::RichText::new(&line.text).small().weak().italics(),
                                );
                            }
                            Kind::Suggestion => {
                                // Boxed and labelled, so a suggestion is never
                                // mistaken for something a person said.
                                egui::Frame::new()
                                    .fill(egui::Color32::from_rgba_unmultiplied(30, 38, 52, 200))
                                    .inner_margin(6.0)
                                    .corner_radius(4.0)
                                    .show(ui, |ui| {
                                        ui.label(
                                            egui::RichText::new("jay suggests")
                                                .small()
                                                .strong()
                                                .color(egui::Color32::from_rgb(150, 190, 255)),
                                        );
                                        ui.label(&line.text);
                                    });
                                ui.add_space(4.0);
                            }
                            Kind::Transcript => {
                                ui.horizontal_wrapped(|ui| {
                                    ui.spacing_mut().item_spacing.x = 4.0;
                                    ui.label(
                                        egui::RichText::new(format!("{}:", line.speaker))
                                            .strong()
                                            .color(speaker_colour(&line.speaker)),
                                    );
                                    ui.label(&line.text);
                                    ui.label(
                                        egui::RichText::new(format!("{:.1}s", line.lag.as_secs_f32()))
                                            .small()
                                            .weak(),
                                    );
                                });
                            }
                        }
                    }
                });
        }

        // Repaint steadily rather than only on input, or new transcript lines
        // would sit in the channel until the mouse happened to move.
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(100));
    }
}

fn speaker_colour(speaker: &str) -> egui::Color32 {
    match speaker {
        "you" => egui::Color32::from_rgb(140, 200, 160),
        _ => egui::Color32::from_rgb(220, 180, 130),
    }
}
