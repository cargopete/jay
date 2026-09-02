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

mod flowchart;

/// How many lines the panel keeps. Older ones scroll off and are forgotten by
/// the UI; the transcript itself is not the UI's job to store.
const MAX_LINES: usize = 200;

/// How many finished answers the reading pane stacks before dropping the
/// oldest. A long round is a dozen presses; twenty is more than anyone scrolls
/// back through and still bounded.
const MAX_ANSWERS: usize = 20;

/// What sort of line this is, which decides how it is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Something somebody said.
    Transcript,
    /// Something jay is offering. Drawn distinctly, because you should never
    /// have to wonder whether you are reading a person or a machine.
    Suggestion,
    /// An answer still being written. Replaces the last partial rather than
    /// accumulating, and is discarded the moment the finished one lands.
    ///
    /// The whole answer takes about fourteen seconds and the first words take
    /// about five. Showing them at five is the difference between an answer
    /// you can still use and one that arrives after the moment.
    Partial,
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
    /// Seconds since the session started, for the archived transcript.
    ///
    /// Reading a session back, "four minutes of silence here" is often the
    /// most informative thing in it.
    ///
    /// Stamped by whoever *made* the line, from when the speech began — not
    /// from when the transcript of it arrived. Those differ by the VAD's
    /// silence hangover plus however long whisper took, which is seconds, and
    /// the gap is not constant. A stamp taken on arrival is a stamp of the
    /// decoder's mood.
    ///
    /// `None` means nobody stamped it and the archiver should use the time it
    /// arrived. An `Option` rather than a zero, because zero is a real time —
    /// the first notice of a session lands on it — and a sentinel that
    /// collides with a legitimate value is a sentinel that eventually
    /// rewrites one.
    pub at: Option<std::time::Duration>,
    /// How long the utterance lasted. Zero for anything that was not speech.
    pub spoken: std::time::Duration,
    /// Heard, but not trusted.
    ///
    /// The transcript keeps it and the model's context does not. A record of a
    /// meeting that silently omits the quiet half of it is worse than one that
    /// admits which lines it is unsure about.
    pub uncertain: bool,
}

impl Line {
    pub fn transcript(speaker: impl Into<String>, text: impl Into<String>, lag: std::time::Duration) -> Self {
        Self {
            speaker: speaker.into(),
            text: text.into(),
            lag,
            kind: Kind::Transcript,
            at: None,
            spoken: std::time::Duration::ZERO,
            uncertain: false,
        }
    }

    pub fn suggestion(text: impl Into<String>, lag: std::time::Duration) -> Self {
        Self {
            speaker: "jay".into(),
            text: text.into(),
            lag,
            kind: Kind::Suggestion,
            at: None,
            spoken: std::time::Duration::ZERO,
            uncertain: false,
        }
    }

    /// An answer in progress. `text` is the whole of it so far.
    pub fn partial(text: impl Into<String>) -> Self {
        Self {
            speaker: "jay".into(),
            text: text.into(),
            lag: std::time::Duration::ZERO,
            kind: Kind::Partial,
            at: None,
            spoken: std::time::Duration::ZERO,
            uncertain: false,
        }
    }

    pub fn notice(text: impl Into<String>) -> Self {
        Self {
            speaker: "jay".into(),
            text: text.into(),
            lag: std::time::Duration::ZERO,
            kind: Kind::Notice,
            at: None,
            spoken: std::time::Duration::ZERO,
            uncertain: false,
        }
    }

    /// Stamp when this happened, relative to the start of the session.
    #[must_use]
    pub fn at(mut self, elapsed: std::time::Duration) -> Self {
        self.at = Some(elapsed);
        self
    }

    /// Say how long it took to say.
    #[must_use]
    pub fn spoken(mut self, duration: std::time::Duration) -> Self {
        self.spoken = duration;
        self
    }

    /// Mark it heard but not trusted.
    #[must_use]
    pub fn uncertain(mut self) -> Self {
        self.uncertain = true;
        self
    }
}

/// Something the panel asks the pipeline to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    /// The user wants a suggestion now, about whatever is going on.
    ///
    /// The most valuable trigger there is: no false positives, nothing spent
    /// that was not asked for, and the moment of the click is exactly the
    /// moment the screen shows the thing being discussed.
    Suggest,
    /// Change what kind of answer the lever gives.
    ///
    /// A mock loop is an algorithmic round followed by a design round, and
    /// before this the only way between them was to quit and relaunch — which
    /// means quitting *during an interview*, losing the transcript that had
    /// just been built up, and fumbling a terminal while someone waits.
    SetMode(jay_agent::Mode),
    /// Switch between the worked answer and a nudge.
    SetDepth(jay_agent::Depth),
}

/// Run the overlay. Blocks until the window is closed.
// Nine things the panel cannot work out for itself, and a struct would move the
// list somewhere else rather than shorten it. See the note on `Overlay::new`.
#[allow(clippy::too_many_arguments)]
pub fn run(
    rx: Receiver<Line>,
    requests: crossbeam_channel::Sender<Request>,
    model_name: String,
    mode: jay_agent::Mode,
    depth: jay_agent::Depth,
    levels: std::sync::Arc<jay_audio::Levels>,
    // Set by the pipeline when it stops on its own — the clock ran out, or the
    // capture died. Without it a timed session leaves the panel sitting there
    // looking like it is still recording, and because the notes are only
    // written after this function returns, they are never written at all. A
    // 120-second session was observed still holding the window four minutes
    // later with its transcript complete and no notes beside it.
    finished: std::sync::Arc<std::sync::atomic::AtomicBool>,
    // `expected` is which channels the session asked for, as `[mic, system]`.
    // Without it the panel cannot tell a channel that was never switched on
    // from one that was switched on and is delivering nothing, and those are
    // opposite problems: the first is correct, the second is the whole reason
    // the meters exist.
    expected: [bool; 2],
) -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("jay")
            .with_inner_size([620.0, COMPACT_HEIGHT])
            .with_min_inner_size([360.0, 200.0])
            .with_transparent(true)
            .with_decorations(false)
            .with_always_on_top(),
        ..Default::default()
    };

    eframe::run_native(
        "jay",
        options,
        Box::new(move |cc| {
            Ok(Box::new(Overlay::new(
                cc, rx, requests, model_name, mode, depth, levels, finished, expected,
            )))
        }),
    )
}

/// What one channel's meter is doing, resolved fresh each repaint.
struct Reading {
    /// Meter travel, 0 to 1, already decayed for display.
    fraction: f32,
    /// Frames are still arriving. False means the capture thread has stopped
    /// or was never running, which is a different fault from a quiet room and
    /// must not be drawn the same way.
    running: bool,
    /// The VAD currently has this channel open.
    speaking: bool,
}

struct Overlay {
    rx: Receiver<Line>,
    requests: crossbeam_channel::Sender<Request>,
    lines: VecDeque<Line>,
    model_name: String,
    /// What the lever will give, and how much of it. Held here so the switches
    /// light correctly the instant they are thrown rather than after the
    /// pipeline has acknowledged them.
    mode: jay_agent::Mode,
    depth: jay_agent::Depth,
    started: Instant,
    /// Set when a suggestion is in flight, so the button can say so. A
    /// suggestion takes twelve seconds at best; a button that looks idle for
    /// twelve seconds gets pressed again.
    waiting_since: Option<Instant>,
    /// When speech last arrived. Powers the intake lamp, which reports
    /// liveness and is therefore lit by the thing it reports on.
    last_signal: Option<Instant>,
    /// The answer currently being written, if one is.
    partial: Option<String>,
    /// Every finished answer this session, oldest first.
    ///
    /// Kept apart from the transcript because these are the things being read,
    /// and the transcript is the thing that moves. Stacked rather than replaced
    /// because the follow-up questions in an interview are about the answer
    /// before last as often as the last one, and a panel that threw the
    /// previous answer away the moment a new one arrived made you choose
    /// between reading and asking.
    answers: Vec<Line>,
    /// Set when a new answer arrives, to bring its first line into view.
    rewind: bool,
    /// Whether the window has already grown for its first answer.
    expanded: bool,
    /// Live input levels, written by the capture loop.
    levels: std::sync::Arc<jay_audio::Levels>,
    /// Which channels this session asked for, as `[mic, system]`.
    expected: [bool; 2],
    /// Set by the pipeline when it stops on its own. Closes the window, which
    /// is what releases the notes.
    finished: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Displayed needle position per channel, and the frame count it was taken
    /// at. Held so the meter can fall smoothly rather than flickering at the
    /// frame rate, and so a stalled stream can be told from a silent one.
    shown: [(f32, u64); 2],
}

impl Overlay {
    // Nine arguments, and each is a distinct thing the panel cannot work out
    // for itself: what to draw, where to send presses, which model, which
    // switch positions, the live levels, whether the pipeline has stopped, and
    // which channels were asked for. A struct would move the list somewhere
    // else without shortening it.
    #[allow(clippy::too_many_arguments)]
    fn new(
        cc: &eframe::CreationContext<'_>,
        rx: Receiver<Line>,
        requests: crossbeam_channel::Sender<Request>,
        model_name: String,
        mode: jay_agent::Mode,
        depth: jay_agent::Depth,
        levels: std::sync::Arc<jay_audio::Levels>,
        finished: std::sync::Arc<std::sync::atomic::AtomicBool>,
        expected: [bool; 2],
    ) -> Self {
        // Dark, translucent, and low contrast enough to sit over a terminal
        // without becoming the thing you look at.
        let mut visuals = egui::Visuals::dark();
        visuals.panel_fill = IRON;
        visuals.window_fill = IRON;
        visuals.override_text_color = Some(INK);
        visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, SEAM);
        cc.egui_ctx.set_visuals(visuals);

        // Monospace is the body face, not an accent: an instrument panel is
        // set in the face the machine can print.
        cc.egui_ctx.all_styles_mut(|style| {
            for id in style.text_styles.values_mut() {
                id.family = egui::FontFamily::Monospace;
            }
        });

        Self {
            rx,
            requests,
            lines: VecDeque::with_capacity(MAX_LINES),
            model_name,
            mode,
            depth,
            started: Instant::now(),
            waiting_since: None,
            last_signal: None,
            partial: None,
            answers: Vec::new(),
            rewind: false,
            expanded: false,
            levels,
            expected,
            finished,
            shown: [(0.0, 0); 2],
        }
    }

    /// Read one channel's meter, decaying the displayed value toward it.
    ///
    /// The fall is deliberately slower than the rise: a meter that drops
    /// instantly to zero between syllables reads as broken, and the thing being
    /// answered here is "is it hearing me", which wants the envelope of speech
    /// rather than the instantaneous sample.
    fn reading(&mut self, channel: jay_audio::Channel, slot: usize) -> Reading {
        let (rms, frames, speaking) = self.levels.meter(channel).read();
        let target = jay_audio::meter_fraction(rms);
        let (previous, seen_at) = self.shown[slot];
        let running = frames > seen_at;
        // Nothing arriving: fall to nothing. Do not hold the last value, which
        // would leave a dead stream showing a healthy bar.
        let fraction = if running {
            target.max(previous - 0.06)
        } else {
            (previous - 0.06).max(0.0)
        };
        self.shown[slot] = (fraction, frames);
        Reading {
            fraction,
            running,
            speaking,
        }
    }

    /// One switch: a word that lights when it is the one in use.
    ///
    /// Deliberately not a button. A button is a thing you press to make
    /// something happen; these are positions of a switch, and only one of each
    /// bank can be thrown at a time. Lit brass is the position it is in.
    fn switch(ui: &mut egui::Ui, label: &str, active: bool) -> bool {
        let text = egui::RichText::new(label.to_uppercase())
            .size(LABEL)
            .family(egui::FontFamily::Monospace)
            .color(if active { BRASS_HI } else { INK_FAINT });
        let response = ui.add(egui::Label::new(text).sense(egui::Sense::click()));
        if response.hovered() && !active {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        response.clicked() && !active
    }

    /// A latching toggle, unlike [`switch`](Self::switch), which is a radio
    /// and refuses the position it is already in.
    ///
    /// Ember when engaged, because muted is not a neutral state: it is jay not
    /// recording somebody, and it must look like a thing that is on.
    fn toggle(ui: &mut egui::Ui, label: &str, engaged: bool) -> bool {
        let text = egui::RichText::new(label.to_uppercase())
            .size(LABEL)
            .family(egui::FontFamily::Monospace)
            .color(if engaged { EMBER } else { INK_FAINT });
        let response = ui
            .add(egui::Label::new(text).sense(egui::Sense::click()))
            .on_hover_text(if engaged {
                "recording this channel again"
            } else {
                "stop recording this channel"
            });
        if response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        response.clicked()
    }

    /// The reading compartment. `lag` present means finished; absent means
    /// still being written, which is drawn in ember rather than brass.
    fn draw_reading(ui: &mut egui::Ui, text: &str, lag: Option<std::time::Duration>) {
        egui::Frame::new()
            .fill(IRON_2)
            .stroke(egui::Stroke::new(1.0, SEAM))
            .inner_margin(10.0)
            .corner_radius(2.0)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("READING")
                            .size(HEADING)
                            .strong()
                            // Ember is hot iron, which is what working looks
                            // like; brass is the finished reading.
                            .color(if lag.is_some() { BRASS } else { EMBER }),
                    );
                    ui.with_layout(
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            ui.label(stencil(&match lag {
                                Some(lag) => format!("{:.0}s", lag.as_secs_f32()),
                                None => "writing".to_string(),
                            }));
                            // Whole answer, prose and all. The per-block button
                            // below is for pasting code straight into an
                            // editor; this is for keeping the reasoning.
                            if lag.is_some() {
                                ui.add_space(8.0);
                                if Self::copy_button(ui, "copy all") {
                                    ui.ctx().copy_text(text.to_string());
                                }
                            }
                        },
                    );
                });
                ui.add_space(3.0);
                Self::draw_answer(ui, text);
            });
    }

    /// Draw an answer, with its code in a well cut into the plate.
    fn draw_answer(ui: &mut egui::Ui, text: &str) {
        for block in split_blocks(text) {
            match block {
                Block::Prose(prose) => {
                    ui.label(
                        egui::RichText::new(plain(prose)).size(BODY).color(INK),
                    );
                    ui.add_space(4.0);
                }
                // A diagram is drawn, not printed. The source stays one
                // button away for Excalidraw, but eight lines of
                // `A[Clients] -->|HTTPS| N[Nginx x2]` is a diagram somebody
                // else has to render, and mid-interview nobody renders
                // anything.
                Block::Code { lang, body: code }
                    if lang.eq_ignore_ascii_case("mermaid")
                        && flowchart::parse(code).is_some() =>
                {
                    let chart = flowchart::parse(code).expect("just checked");
                    egui::Frame::new()
                        .fill(INSET)
                        .stroke(egui::Stroke::new(1.0, SEAM))
                        .inner_margin(8.0)
                        .corner_radius(2.0)
                        .show(ui, |ui| {
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Min),
                                |ui| {
                                    if Self::copy_button(ui, "copy mermaid") {
                                        ui.ctx().copy_text(code.to_string());
                                    }
                                },
                            );
                            flowchart::draw(ui, &chart, INK, BRASS, IRON_2, INK_FAINT);
                        });
                    ui.add_space(5.0);
                }
                Block::Code { lang, body: code } => {
                    let _ = lang;
                    egui::Frame::new()
                        .fill(INSET)
                        .stroke(egui::Stroke::new(1.0, SEAM))
                        .inner_margin(8.0)
                        .corner_radius(2.0)
                        .show(ui, |ui| {
                            // The button sits above the code rather than
                            // floating over it: an overlay would cover the
                            // first line, which is the one being read.
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Min),
                                |ui| {
                                    if Self::copy_button(ui, "copy") {
                                        ui.ctx().copy_text(code.to_string());
                                    }
                                },
                            );
                            // Selectable as well as copyable, so a single
                            // function can be lifted out of a longer block.
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(code)
                                        .size(CODE)
                                        .family(egui::FontFamily::Monospace)
                                        .color(BRASS_HI),
                                )
                                .selectable(true),
                            );
                        });
                    ui.add_space(5.0);
                }
            }
        }
    }

    /// A small unlit button in the same idiom as the switches.
    fn copy_button(ui: &mut egui::Ui, label: &str) -> bool {
        ui.add(
            egui::Button::new(
                egui::RichText::new(label)
                    .size(LABEL)
                    .family(egui::FontFamily::Monospace)
                    .color(INK_FAINT),
            )
            .fill(IRON_2)
            .stroke(egui::Stroke::new(1.0, SEAM))
            .corner_radius(2.0),
        )
        .on_hover_text("to the clipboard")
        .clicked()
    }

    /// The switch bank: which round the lever is answering for, and whether it
    /// hands over the answer or a nudge.
    fn draw_switches(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            ui.label(stencil("round"));
            for mode in jay_agent::Mode::ALL {
                if Self::switch(ui, mode.label(), mode == self.mode)
                    && self.requests.send(Request::SetMode(mode)).is_ok()
                {
                    self.mode = mode;
                }
            }

            // Laid out left to right like everything else. A right-to-left
            // layout put this group in reverse — "NUDGE ANSWER GIVES" — which
            // is the sort of thing that is obvious the moment somebody looks
            // at it and invisible to anyone who cannot.
            // "gives" read as a verb with no subject and nobody knew what it
            // meant, including the person who uses this every day. The switch
            // chooses how much of an answer arrives, so it says so.
            ui.add_space(10.0);
            ui.label(stencil("detail"));
            for depth in jay_agent::Depth::ALL {
                if Self::switch(ui, depth.label(), depth == self.depth)
                    && self.requests.send(Request::SetDepth(depth)).is_ok()
                {
                    self.depth = depth;
                }
            }
        });
    }

    /// One channel's input meter: a track cut into the plate, with a fill.
    ///
    /// A gauge here earns its place by the ornament test — it is bound to the
    /// actual RMS of the actual samples, and it is the only thing in the panel
    /// that reports on the ten seconds between a sound arriving and a sentence
    /// appearing.
    fn draw_meter(
        &mut self,
        ui: &mut egui::Ui,
        label: &str,
        channel: jay_audio::Channel,
        slot: usize,
    ) {
        let reading = self.reading(channel, slot);
        // A channel that has never delivered a single frame was never switched
        // on. That is not a fault, and must not be drawn as one.
        let ever_ran = self.shown[slot].1 > 0;
        let colour = match channel {
            jay_audio::Channel::Mic => BRASS,
            jay_audio::Channel::System => COPPER,
        };

        ui.horizontal(|ui| {
            ui.label(stencil(label));

            // Right to left, so the words on the right are placed first and
            // the bar takes whatever is left over.
            //
            // This used to reserve a fixed 78 points for the status word and
            // give the bar the rest, which was fine while the word was the
            // last thing on the row. Adding the mute switch after it pushed
            // past the edge of the panel and the switch rendered as "MU" —
            // present, unreadable, and mostly unclickable. A hard-coded width
            // for text of varying length is a bug waiting for the day
            // somebody adds one more word, and that day was the same
            // afternoon.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            // The word beside the meter is the diagnosis. The two channels
            // need different words, because absence means opposite things on
            // them and calling both "stalled" cries wolf once a minute.
            //
            // A live microphone delivers frames whether or not anyone is
            // speaking — measured at 248 frames in a silent room — so silence
            // there is genuinely a fault and is drawn as one.
            //
            // A CoreAudio process tap delivers no callbacks at all when the
            // output is idle. Not silent frames: none. So a quiet call and a
            // dead tap are indistinguishable from here and always will be, and
            // painting that in ember would be a fault light wired to a pause
            // in the conversation. `jay check`, with something playing, is the
            // instrument that can actually tell the difference.
            let waited = self.started.elapsed() > SETTLE;
            let live_mic = channel == jay_audio::Channel::Mic;
            let muted = self.levels.meter(channel).is_muted();

            // The switch sits on the channel it silences rather than in the
            // bank below, because "which one does this mute" should not be a
            // question anybody has to ask mid-meeting.
            if self.expected[slot] && Self::toggle(ui, "mute", muted) {
                self.levels.meter(channel).set_muted(!muted);
            }

            let (word, tint) = if muted {
                // Outranks every diagnosis below. A muted channel is not
                // stalled, not quiet and not at fault; it is switched off on
                // purpose, and the bar beside this word is still moving to
                // prove the audio is arriving.
                ("muted", EMBER)
            } else {
                match (self.expected[slot], ever_ran, reading.running) {
                // Never asked for. Correct, and not a fault.
                (false, _, _) => ("off", INK_FAINT),
                (true, true, true) if reading.speaking => ("speech", VERDIGRIS),
                (true, true, true) => ("quiet", INK_FAINT),
                (true, _, false) if !waited => ("starting", INK_FAINT),
                // Delivered, then stopped.
                (true, true, false) if live_mic => ("stalled", EMBER),
                (true, true, false) => ("idle", INK_FAINT),
                // Asked for, and has never delivered a single frame.
                (true, false, _) if live_mic => ("no frames", EMBER),
                (true, false, _) => ("no audio yet", INK_FAINT),
                }
            };
            ui.label(
                egui::RichText::new(word.to_uppercase())
                    .size(LABEL)
                    .family(egui::FontFamily::Monospace)
                    .color(tint),
            );

            // Whatever is left after the words. The bar is the one thing on
            // the row that can afford to be short.
            let width = ui.available_width().max(40.0);
            let (rect, _) =
                ui.allocate_exact_size(egui::vec2(width, 9.0), egui::Sense::hover());
            ui.painter().rect_filled(rect, 2.0, INSET);
            ui.painter().rect_stroke(
                rect,
                2.0,
                egui::Stroke::new(1.0, SEAM),
                egui::StrokeKind::Inside,
            );
            if reading.fraction > 0.0 {
                let mut fill = rect.shrink(1.5);
                fill.set_width((rect.width() - 3.0) * reading.fraction);
                ui.painter().rect_filled(fill, 1.0, colour);
            }
            });
        });
    }
}

// Palette from the steampunk-ui doctrine: soot and lamp-oil surfaces, warm
// ink, three signal metals with three distinct jobs, and status colours taken
// from what those metals do when they corrode rather than imported from a
// framework. Brass, not gold.

/// Panel ground. `--iron`: this is a plate, not the page.
const IRON: egui::Color32 = egui::Color32::from_rgb(0x1c, 0x18, 0x13);
/// `--inset`: below the plate. Meter tracks are channels cut into it.
const INSET: egui::Color32 = egui::Color32::from_rgb(0x0a, 0x08, 0x04);
/// `--iron-2`: header strips and the compartment a reading sits in.
const IRON_2: egui::Color32 = egui::Color32::from_rgb(0x24, 0x1f, 0x18);
/// `--seam`: hairlines.
const SEAM: egui::Color32 = egui::Color32::from_rgb(0x3a, 0x32, 0x27);
/// `--seam-lit`: structural borders.
const SEAM_LIT: egui::Color32 = egui::Color32::from_rgb(0x4d, 0x42, 0x31);
/// `--ink`: body text.
const INK: egui::Color32 = egui::Color32::from_rgb(0xe8, 0xdf, 0xcd);
/// `--ink-faint`: labels, units, captions.
const INK_FAINT: egui::Color32 = egui::Color32::from_rgb(0x9d, 0x8f, 0x75);
/// `--copper`: **data**. What was said, and who said it.
const COPPER: egui::Color32 = egui::Color32::from_rgb(0xcd, 0x7c, 0x48);
/// `--brass`: **structure**. Labels, framing, the accent.
const BRASS: egui::Color32 = egui::Color32::from_rgb(0xc9, 0xa2, 0x27);
/// `--brass-hi`: **attention**. Scarce by rule — under five percent of surface.
const BRASS_HI: egui::Color32 = egui::Color32::from_rgb(0xf0, 0xd8, 0x91);
/// Not a text colour: engraved edges and the lever's border.
const BRASS_LO: egui::Color32 = egui::Color32::from_rgb(0x7d, 0x63, 0x18);
/// Copper carbonate. Live, receiving.
const VERDIGRIS: egui::Color32 = egui::Color32::from_rgb(0x5f, 0x9e, 0x7d);
/// Hot iron. Working.
const EMBER: egui::Color32 = egui::Color32::from_rgb(0xe8, 0x92, 0x2a);

/// Alpha of the window. Enough to read against anything, with just enough
/// translucency to remember it is sitting on top of something.
///
/// This started fully transparent, on the theory that an overlay should blend.
/// eframe 0.36's `ui()` API paints no background of its own — the
/// `CentralPanel` used to do it — so "blend" meant bare text floating over a
/// terminal, completely unreadable. A panel you cannot read is not a subtle
/// panel.
const GROUND_ALPHA: f32 = 0.97;

/// The panel with nothing to read in it: header, meters, switches, and enough
/// transcript to see the conversation moving.
///
/// It opens at this height and grows the first time there is an answer. An
/// empty 620x620 slab parked over your editor is the opposite of glanceable,
/// and the panel spends most of a session with nothing in the reading pane.
/// It never shrinks again on its own — moving a window somebody is reading is
/// worse than leaving it large.
const COMPACT_HEIGHT: f32 = 330.0;

/// The panel with an answer in it.
const EXPANDED_HEIGHT: f32 = 640.0;

/// How long a channel may take to deliver its first frame before the panel
/// calls it dead. A cpal stream takes a moment to open; a minute does not.
const SETTLE: std::time::Duration = std::time::Duration::from_secs(6);

/// How recently a line must have arrived for the intake lamp to be lit.
///
/// The lamp reports liveness and is powered by the thing it reports on: lit
/// when speech is arriving, dark when it is not. It is never green-for-nothing.
const LIVE_WINDOW: std::time::Duration = std::time::Duration::from_secs(12);

// Type scale. Bimodal on purpose — small tracked labels against large
// readings, with almost nothing between, which is where the instrument-panel
// feel lives. Both ends are larger than a web page would use: this is a small
// panel read at a glance, mid-sentence, while someone is waiting for you.

/// Transcript lines and the body of a reading. The size that matters.
const BODY: f32 = 16.0;
/// Stencilled labels: units, captions, the model name.
const LABEL: f32 = 11.0;
/// Speaker attribution and compartment headings.
const HEADING: f32 = 13.0;
/// The nameplate.
const NAMEPLATE: f32 = 16.0;
/// Code in a reading. A shade under the body, because a line of Rust is longer
/// than a line of prose and wrapping code is worse than reading it small.
const CODE: f32 = 14.0;

/// A run of the answer, split so code can be drawn as code.
#[derive(Debug, PartialEq, Eq)]
enum Block<'a> {
    Prose(&'a str),
    /// A fenced block and its language tag, which decides whether it is drawn
    /// as code or as a picture.
    Code { lang: &'a str, body: &'a str },
}

/// Split an answer on fenced code blocks.
///
/// The model returns markdown and the panel renders none of it, so before this
/// a coding answer arrived with its fences and asterisks intact — read at a
/// glance, mid-sentence, with somebody waiting. Code is the thing you are
/// looking for in that panel and it should be the thing your eye lands on.
fn split_blocks(text: &str) -> Vec<Block<'_>> {
    let mut blocks = Vec::new();
    let mut rest = text;

    while let Some(open) = rest.find("```") {
        let (before, after) = rest.split_at(open);
        if !before.trim().is_empty() {
            blocks.push(Block::Prose(before.trim_matches('\n')));
        }
        let after = &after[3..];
        let (lang, body) = match after.find('\n') {
            Some(nl) => (after[..nl].trim(), &after[nl + 1..]),
            None => ("", ""),
        };
        match body.find("```") {
            Some(close) => {
                blocks.push(Block::Code {
                    lang,
                    body: body[..close].trim_end_matches('\n'),
                });
                rest = &body[close + 3..];
            }
            // An unterminated fence: everything left is code. Happens on every
            // partial, since the answer is drawn while it is still being
            // written and the closing fence has not arrived yet.
            None => {
                if !body.trim().is_empty() {
                    blocks.push(Block::Code {
                        lang,
                        body: body.trim_end_matches('\n'),
                    });
                }
                return blocks;
            }
        }
    }

    if !rest.trim().is_empty() {
        blocks.push(Block::Prose(rest.trim_matches('\n')));
    }
    blocks
}

/// Strip the markdown emphasis nothing here renders.
fn plain(text: &str) -> String {
    text.replace("**", "")
}

/// A tracked uppercase label, the way a stencil does it.
fn stencil(text: &str) -> egui::RichText {
    egui::RichText::new(text.to_uppercase())
        .size(LABEL)
        .family(egui::FontFamily::Monospace)
        .color(INK_FAINT)
}

impl eframe::App for Overlay {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [
            IRON.r() as f32 / 255.0,
            IRON.g() as f32 / 255.0,
            IRON.b() as f32 / 255.0,
            GROUND_ALPHA,
        ]
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Checked before anything is drawn. The pipeline has stopped and the
        // notes are waiting on this function returning, so there is nothing to
        // be gained by holding the window open for one more frame.
        if self.finished.load(std::sync::atomic::Ordering::Relaxed) {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }

        for line in self.rx.try_iter() {
            // A partial replaces the one before it and is never retained: the
            // finished suggestion is the thing that goes in the transcript.
            if line.kind == Kind::Partial {
                if self.partial.is_none() {
                    // A fresh answer starts at its beginning.
                    self.rewind = true;
                }
                self.partial = Some(line.text);
                continue;
            }
            // A suggestion arriving is what clears the waiting state.
            if line.kind == Kind::Suggestion {
                self.waiting_since = None;
                self.partial = None;
                if self.answers.len() == MAX_ANSWERS {
                    self.answers.remove(0);
                }
                self.answers.push(line.clone());
                self.rewind = true;
            }
            if line.kind == Kind::Transcript {
                self.last_signal = Some(Instant::now());
            }
            if self.lines.len() == MAX_LINES {
                self.lines.pop_front();
            }
            self.lines.push_back(line);
        }

        // Paint the ground and a border explicitly. Nothing else does: with no
        // `CentralPanel` there is no background, and an undecorated window with
        // no edge is impossible to find on a busy desktop.
        let full = ui.max_rect();
        ui.painter().rect_filled(full, 3.0, IRON);
        // Machined, not moulded: 3px on a plate. A lit hairline along the top
        // edge, because every plate here is lit from above.
        ui.painter()
            .rect_stroke(full, 3.0, egui::Stroke::new(1.0, SEAM_LIT), egui::StrokeKind::Inside);
        ui.painter().line_segment(
            [full.left_top() + egui::vec2(3.0, 0.5), full.right_top() + egui::vec2(-3.0, 0.5)],
            egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(0xf0, 0xd8, 0x91, 18)),
        );

        {
            // The header doubles as a drag handle: there is no title bar, and
            // a window you cannot move is a window in the way.
            let header = ui.horizontal(|ui| {
                // The nameplate. Engraved, and there is exactly one.
                ui.label(
                    egui::RichText::new("JAY")
                        .size(NAMEPLATE)
                        .strong()
                        .color(BRASS_HI),
                );

                // Intake lamp: lit while speech is arriving, dark when it is
                // not. Never lit for nothing.
                let live = self
                    .last_signal
                    .is_some_and(|at| at.elapsed() < LIVE_WINDOW);
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(9.0, 9.0), egui::Sense::hover());
                let centre = rect.center();
                if live {
                    ui.painter()
                        .circle_filled(centre, 4.5, VERDIGRIS.gamma_multiply(0.28));
                    ui.painter().circle_filled(centre, 2.5, VERDIGRIS);
                } else {
                    ui.painter()
                        .circle_stroke(centre, 2.5, egui::Stroke::new(1.0, SEAM_LIT));
                }
                ui.label(stencil(if live { "intake" } else { "no signal" }));

                ui.label(stencil(&self.model_name));

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("×").clicked() {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                    }

                    // Minimise, because the only way to get the panel out of
                    // the way used to be to end the recording. The window is
                    // undecorated, so there is no system chrome to borrow one
                    // from and it has to be drawn here.
                    //
                    // Deliberately not labelled with anything that reads as
                    // stopping: the session goes on recording while it is
                    // minimised, and a control that looks like a stop button
                    // but is not is worse than no control at all.
                    if ui
                        .small_button("–")
                        .on_hover_text("hide the panel; the session keeps recording")
                        .clicked()
                    {
                        ui.ctx()
                            .send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                    }

                    match self.waiting_since {
                        // Working. Ember, because hot iron is what working
                        // looks like, and the figure is real.
                        Some(since) => {
                            ui.label(
                                egui::RichText::new(format!(
                                    "WORKING {:.0}s",
                                    since.elapsed().as_secs_f32()
                                ))
                                .size(HEADING)
                                .strong()
                                .color(EMBER),
                            );
                        }
                        // The lever. One per panel, brass plate, dark text
                        // struck into the metal.
                        None => {
                            let lever = egui::Button::new(
                                egui::RichText::new("ASK JAY")
                                    .size(HEADING)
                                    .strong()
                                    .color(egui::Color32::from_rgb(0x24, 0x1c, 0x05)),
                            )
                            .fill(BRASS)
                            .stroke(egui::Stroke::new(1.0, BRASS_LO))
                            .corner_radius(2.0);
                            if ui.add(lever).clicked()
                                && self.requests.send(Request::Suggest).is_ok()
                            {
                                self.waiting_since = Some(Instant::now());
                            }
                        }
                    }

                    // Elapsed is a reading, so it is copper.
                    let elapsed = self.started.elapsed().as_secs();
                    ui.label(
                        egui::RichText::new(format!("{:02}:{:02}", elapsed / 60, elapsed % 60))
                            .size(HEADING)
                            .color(COPPER),
                    );
                });
            });

            if header.response.interact(egui::Sense::drag()).dragged() {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
            }

            ui.separator();

            // Intake meters. These sit above the transcript because they
            // answer the question the transcript cannot: an empty transcript
            // means either nobody spoke or nothing was heard, and until there
            // was a meter here those two looked identical.
            ui.add_space(2.0);
            self.draw_meter(ui, "you", jay_audio::Channel::Mic, 0);
            ui.add_space(3.0);
            self.draw_meter(ui, "them", jay_audio::Channel::System, 1);
            ui.add_space(4.0);
            ui.separator();
            ui.add_space(3.0);
            self.draw_switches(ui);
            ui.add_space(4.0);
            ui.separator();

            // Two compartments, and which is which matters.
            //
            // One scroll area held everything, sticking to the bottom, so an
            // answer was read from its end backwards: the "Approach:" line —
            // the one sentence you are meant to say out loud first — had
            // already scrolled off by the time the code finished arriving, and
            // every transcript line that landed while you read pushed it
            // further away.
            //
            // So the answer gets the top and stays where it was put, and the
            // conversation runs along the bottom where it can chase itself.
            // With nothing to read, the reading pane is a label and no more;
            // the conversation takes the rest.
            let has_reading = self.partial.is_some() || !self.answers.is_empty();
            let split = if has_reading {
                (ui.available_height() * 0.66).max(140.0)
            } else {
                46.0
            };

            let reading = egui::ScrollArea::vertical()
                .id_salt("reading")
                .max_height(split)
                .auto_shrink([false, false]);
            let rewind = std::mem::take(&mut self.rewind);
            reading.show(ui, |ui| {
                let newest = self.answers.len().saturating_sub(1);
                for (i, answer) in self.answers.iter().enumerate() {
                    // Bring the newest answer's first line to the top of the
                    // view, leaving the earlier ones above it to scroll back
                    // to. Scrolling the whole pane to zero would land on the
                    // oldest answer instead, which is the one nobody wants.
                    if rewind && i == newest && self.partial.is_none() {
                        ui.scroll_to_cursor(Some(egui::Align::TOP));
                    }
                    Self::draw_reading(ui, &answer.text, Some(answer.lag));
                    ui.add_space(6.0);
                }
                match (&self.partial, self.answers.is_empty()) {
                    (Some(partial), _) => {
                        if rewind {
                            ui.scroll_to_cursor(Some(egui::Align::TOP));
                        }
                        Self::draw_reading(ui, partial, None);
                    }
                    (None, false) => {}
                    (None, true) => {
                        // Absent is absent. Never a zero, never a dash that
                        // could pass for a reading.
                        ui.add_space(8.0);
                        ui.label(stencil("no reading"));
                        ui.label(
                            egui::RichText::new("nothing asked for yet")
                                .size(BODY)
                                .color(INK_FAINT),
                        );
                    }
                }
            });

            ui.add_space(4.0);
            ui.separator();
            ui.add_space(3.0);

            egui::ScrollArea::vertical()
                .id_salt("transcript")
                .stick_to_bottom(true)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    if self.lines.is_empty() {
                        ui.label(stencil("no signal"));
                        ui.label(
                            egui::RichText::new("nothing heard yet")
                                .size(BODY)
                                .color(INK_FAINT),
                        );
                    }
                    for line in &self.lines {
                        match line.kind {
                            Kind::Notice => {
                                // Not a stencil. Uppercase with wide tracking
                                // is how a machine labels a dial; shouting a
                                // whole sentence in it — "I WILL NOT SAY
                                // ANYTHING UNTIL YOU PRESS ASK JAY" — is how a
                                // machine sounds unhinged.
                                ui.label(
                                    egui::RichText::new(&line.text)
                                        .size(LABEL)
                                        .family(egui::FontFamily::Monospace)
                                        .color(INK_FAINT),
                                );
                                ui.add_space(3.0);
                            }
                            // Answers live in the compartment above; drafts of
                            // them are never retained at all.
                            Kind::Partial | Kind::Suggestion => {}
                            Kind::Transcript => {
                                ui.horizontal_wrapped(|ui| {
                                    ui.spacing_mut().item_spacing.x = 7.0;
                                    // Faint, and marked. An unsure line is
                                    // still a line somebody said; drawing it
                                    // as confidently as the rest would be the
                                    // opposite lie to dropping it.
                                    let (ink, name) = if line.uncertain {
                                        (INK_FAINT, format!("{}?", line.speaker.to_uppercase()))
                                    } else {
                                        (INK, line.speaker.to_uppercase())
                                    };
                                    ui.label(
                                        egui::RichText::new(name)
                                            .size(HEADING)
                                            .strong()
                                            .color(speaker_colour(&line.speaker)),
                                    );
                                    ui.label(
                                        egui::RichText::new(&line.text).size(BODY).color(ink),
                                    );
                                });
                                ui.add_space(5.0);
                            }
                        }
                    }
                    // The last line wants room to breathe rather than sitting
                    // against the bottom edge of the plate.
                    ui.add_space(6.0);
                });
        }

        // Grow once, the first time there is something to read. Sent here
        // rather than on arrival because a viewport command needs the context.
        if !self.expanded && (self.partial.is_some() || !self.answers.is_empty()) {
            self.expanded = true;
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::InnerSize(
                egui::vec2(620.0, EXPANDED_HEIGHT),
            ));
        }

        // Repaint steadily rather than only on input, or new transcript lines
        // would sit in the channel until the mouse happened to move.
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(100));
    }
}

/// Who is speaking, in the two signal metals that are not reserved.
///
/// Copper is data — the interviewer, the thing being reacted to. Brass is
/// structure — you, the fixed point the session is organised around. Neither
/// is `--brass-hi`, which is scarce and spent on the working state.
fn speaker_colour(speaker: &str) -> egui::Color32 {
    match speaker {
        "you" => BRASS,
        "jay" => BRASS_HI,
        _ => COPPER,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_answer_splits_into_prose_and_code() {
        let answer = "**Approach:** walk it once.\n\n```rust\nfn f() {}\n```\n\nO(n) time.";
        assert_eq!(
            split_blocks(answer),
            vec![
                Block::Prose("**Approach:** walk it once."),
                Block::Code { lang: "rust", body: "fn f() {}" },
                Block::Prose("O(n) time."),
            ]
        );
    }

    /// Every partial is an unterminated fence, because the panel draws the
    /// answer while it is still being written. If this returned nothing, the
    /// code would appear only at the very end, which is the opposite of what
    /// streaming is for.
    #[test]
    fn a_half_written_code_block_still_draws() {
        let partial = "Approach: walk it.\n\n```rust\nfn f() {\n    let mut prev";
        assert_eq!(
            split_blocks(partial),
            vec![
                Block::Prose("Approach: walk it."),
                Block::Code { lang: "rust", body: "fn f() {\n    let mut prev" },
            ]
        );
    }

    /// The language tag decides whether a block is printed or drawn, so it has
    /// to survive the split.
    #[test]
    fn the_fence_language_is_kept() {
        let answer = "Numbers.\n\n```mermaid\nflowchart TD\nA[a] --> B[b]\n```";
        assert_eq!(
            split_blocks(answer),
            vec![
                Block::Prose("Numbers."),
                Block::Code {
                    lang: "mermaid",
                    body: "flowchart TD\nA[a] --> B[b]"
                },
            ]
        );
    }

    #[test]
    fn prose_without_code_is_left_alone() {
        assert_eq!(
            split_blocks("Just say the thing."),
            vec![Block::Prose("Just say the thing.")]
        );
        assert_eq!(split_blocks(""), vec![]);
    }

    #[test]
    fn emphasis_markers_do_not_reach_the_panel() {
        assert_eq!(plain("**Approach:** walk it"), "Approach: walk it");
    }
}
