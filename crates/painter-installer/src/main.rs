//! PainterSetup.exe: Painter's installer for Windows, in Rust (egui).
//!
//! A welcome page, a tour of the app in slides, what Painter is and its
//! licence, then where to install and what to set up (Start menu, desktop,
//! file types), the install itself and a last page that starts Painter.
//! Everything is for the user alone: no administrator rights.
//!
//! `PainterSetup.exe --uninstall` (what Settings → Apps runs, as
//! `uninstall.exe` in Painter's folder) removes Painter again.
//! `--screenshot FILE --page N` saves page N as a PNG and quits (for checks).

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(windows)]
mod setup;

#[cfg(not(windows))]
fn main() {
    eprintln!("Painter's installer is for Windows; other systems have their own packages.");
}

#[cfg(windows)]
fn main() -> eframe::Result {
    ui::run()
}

#[cfg(windows)]
mod ui {
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use eframe::egui::{self, Align, Color32, CornerRadius, Layout, RichText, Sense, Stroke, Vec2};

    use crate::setup::{self, Options};

    const BACKDROP: Color32 = Color32::from_gray(38);
    const PANEL: Color32 = Color32::from_gray(48);
    const TEXT: Color32 = Color32::from_gray(214);
    const DIM: Color32 = Color32::from_gray(150);
    const ACCENT: Color32 = Color32::from_rgb(98, 132, 196);
    const SELECTED: Color32 = Color32::from_rgb(74, 90, 120);

    /// The tour: picture, title, words (English, Italian).
    const SLIDES: [(&[u8], [&str; 2], [&str; 2]); 5] = [
        (
            include_bytes!("../../../packaging/installer/slides/1-hero.png"),
            ["Paint, ink and lay out comics", "Dipingi, inchiostra e impagina fumetti"],
            [
                "Brushes with pressure, tilt and stabilisation on a GPU canvas that stays smooth on large pages. Layers, masks, blend modes, history.",
                "Pennelli con pressione, inclinazione e stabilizzazione su una tela GPU fluida anche con pagine grandi. Livelli, maschere, metodi di fusione, cronologia.",
            ],
        ),
        (
            include_bytes!("../../../packaging/installer/slides/2-tools.png"),
            ["The tools you know", "Gli strumenti che conosci"],
            [
                "Grouped like Photoshop's, set up like Clip Studio's: selections, fills that close gaps, rulers and perspective, 3D models to pose.",
                "Raggruppati come in Photoshop, impostati come in Clip Studio: selezioni, riempimenti che chiudono i buchi, righelli e prospettiva, modelli 3D da mettere in posa.",
            ],
        ),
        (
            include_bytes!("../../../packaging/installer/slides/3-text.png"),
            ["Text and balloons", "Testo e balloon"],
            [
                "Any font on your computer, vertical text for manga, balloons and tails as in Clip Studio, comic pages and panels.",
                "Tutti i font del computer, testo verticale per i manga, balloon e code come in Clip Studio, pagine e vignette.",
            ],
        ),
        (
            include_bytes!("../../../packaging/installer/slides/4-cmyk.png"),
            ["Ready for print", "Pronto per la stampa"],
            [
                "CMYK through Little CMS, duotone, tones and halftones, printing, and PSD, Krita and Clip Studio files in and out.",
                "CMYK con Little CMS, due tonalità, retini, stampa, e file PSD, Krita e Clip Studio in entrata e in uscita.",
            ],
        ),
        (
            include_bytes!("../../../packaging/installer/slides/5-session.png"),
            ["Draw together", "Disegna insieme"],
            [
                "Shared sessions on your network or over the internet: everyone paints on the same page, with chat.",
                "Sessioni condivise in rete locale o via internet: tutti dipingono sulla stessa pagina, con la chat.",
            ],
        ),
    ];

    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum Page {
        Welcome,
        Tour(usize),
        About,
        Options,
        Working,
        Done,
        Failed,
        // Removing.
        RemoveConfirm,
    }

    #[derive(Default)]
    struct Progress {
        fraction: f32,
        step: &'static str,
        result: Option<Result<(), String>>,
    }

    struct App {
        it: bool,
        page: Page,
        uninstall: Option<PathBuf>,
        opts: Options,
        launch: bool,
        remove_settings: bool,
        slides: Vec<egui::TextureHandle>,
        logo: Option<egui::TextureHandle>,
        slide_since: Instant,
        progress: Arc<Mutex<Progress>>,
        error: String,
        screenshot: Option<(PathBuf, u32)>,
    }

    fn texture(ctx: &egui::Context, name: &str, png: &[u8]) -> Option<egui::TextureHandle> {
        let img = image::load_from_memory(png).ok()?.to_rgba8();
        let size = [img.width() as usize, img.height() as usize];
        Some(ctx.load_texture(name, egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw()), egui::TextureOptions::LINEAR))
    }

    impl App {
        fn t(&self, en: &'static str, it: &'static str) -> &'static str {
            if self.it {
                it
            } else {
                en
            }
        }

        fn start(&mut self, ctx: &egui::Context, removing: bool) {
            self.page = Page::Working;
            *self.progress.lock().unwrap() = Progress::default();
            let progress = self.progress.clone();
            let (opts, dir, settings) = (self.opts.clone(), self.uninstall.clone(), self.remove_settings);
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                let report = |f: f32, step: &'static str| {
                    let mut p = progress.lock().unwrap();
                    p.fraction = f;
                    p.step = step;
                    ctx.request_repaint();
                };
                let result = match (removing, dir) {
                    (true, Some(dir)) => setup::uninstall(&dir, settings, &report),
                    _ => setup::install(&opts, &report),
                };
                progress.lock().unwrap().result = Some(result);
                ctx.request_repaint();
            });
        }

        fn step_name(&self, step: &str) -> &'static str {
            match step {
                "files" => self.t("Copying Painter…", "Copio Painter…"),
                "shortcuts" => self.t("Making shortcuts…", "Creo i collegamenti…"),
                "file types" => self.t("Setting up file types…", "Configuro i tipi di file…"),
                "settings" => self.t("Removing settings…", "Rimuovo le impostazioni…"),
                "apps" => self.t("Updating Settings → Apps…", "Aggiorno Impostazioni → App…"),
                "done" => self.t("Done", "Fatto"),
                _ => self.t("Starting…", "Avvio…"),
            }
        }
    }

    /// A big, rounded button; the main one filled with the accent.
    fn button(ui: &mut egui::Ui, text: &str, main: bool) -> bool {
        let label = RichText::new(text).size(15.0).color(if main { Color32::WHITE } else { TEXT });
        let b = egui::Button::new(label).min_size(Vec2::new(120.0, 34.0)).corner_radius(CornerRadius::same(6)).fill(if main {
            ACCENT
        } else {
            PANEL
        });
        ui.add(b).clicked()
    }

    impl eframe::App for App {
        fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
            if self.slides.is_empty() {
                self.slides = SLIDES.iter().enumerate().filter_map(|(i, s)| texture(ctx, &format!("slide{i}"), s.0)).collect();
                self.logo = texture(ctx, "logo", include_bytes!("../../../packaging/icons/painter-256.png"));
            }
            self.screenshot_tick(ctx);
            // Results from the work thread.
            if self.page == Page::Working {
                let result = self.progress.lock().unwrap().result.take();
                match result {
                    Some(Ok(())) => {
                        self.page = Page::Done;
                        if self.uninstall.is_some() {
                            setup::delete_self_later();
                        }
                    }
                    Some(Err(e)) => {
                        self.error = e;
                        self.page = Page::Failed;
                    }
                    None => {}
                }
            }
            // The tour moves on by itself.
            if let Page::Tour(i) = self.page {
                if self.slide_since.elapsed() > Duration::from_secs(7) {
                    self.page = if i + 1 < SLIDES.len() { Page::Tour(i + 1) } else { Page::About };
                    self.slide_since = Instant::now();
                }
                ctx.request_repaint_after(Duration::from_millis(250));
            }

            egui::TopBottomPanel::bottom("nav")
                .frame(egui::Frame::new().fill(PANEL).inner_margin(egui::Margin::symmetric(20, 14)))
                .show(ctx, |ui| self.nav(ui, ctx));
            egui::CentralPanel::default().frame(egui::Frame::new().fill(BACKDROP).inner_margin(egui::Margin::same(28))).show(
                ctx,
                |ui| match self.page {
                    Page::Welcome => self.welcome(ui),
                    Page::Tour(i) => self.tour(ui, i),
                    Page::About => self.about(ui),
                    Page::Options => self.options(ui),
                    Page::Working => self.working(ui),
                    Page::Done => self.done(ui),
                    Page::Failed => self.failed(ui),
                    Page::RemoveConfirm => self.remove_confirm(ui),
                },
            );
        }
    }

    impl App {
        fn heading(&self, ui: &mut egui::Ui, text: &str) {
            ui.label(RichText::new(text).size(26.0).color(TEXT).strong());
            ui.add_space(8.0);
        }

        fn welcome(&mut self, ui: &mut egui::Ui) {
            ui.vertical_centered(|ui| {
                ui.add_space(30.0);
                if let Some(logo) = &self.logo {
                    ui.add(egui::Image::new(logo).fit_to_exact_size(Vec2::splat(150.0)));
                }
                ui.add_space(14.0);
                ui.label(RichText::new("Painter").size(40.0).color(TEXT).strong());
                ui.label(RichText::new(format!("{} {}", self.t("Version", "Versione"), setup::VERSION)).color(DIM));
                ui.add_space(16.0);
                ui.label(
                    RichText::new(self.t("Paint, ink and lay out comics.", "Dipingi, inchiostra e impagina fumetti."))
                        .size(18.0)
                        .color(TEXT),
                );
                ui.add_space(6.0);
                ui.label(
                    RichText::new(self.t(
                        "This installs Painter for you, without administrator rights. First, a short tour.",
                        "Installa Painter per il tuo utente, senza diritti di amministratore. Prima, un breve giro.",
                    ))
                    .color(DIM),
                );
                if let Some(dir) = setup::installed_dir() {
                    ui.add_space(10.0);
                    ui.label(
                        RichText::new(format!(
                            "{} {}",
                            self.t("Painter is already installed in", "Painter è già installato in"),
                            dir.display()
                        ))
                        .color(ACCENT),
                    );
                }
            });
        }

        fn tour(&mut self, ui: &mut egui::Ui, i: usize) {
            let (_, title, words) = SLIDES[i];
            let k = usize::from(self.it);
            if let Some(tex) = self.slides.get(i) {
                let w = ui.available_width();
                let size = tex.size_vec2() * (w / tex.size_vec2().x).min(330.0 / tex.size_vec2().y);
                ui.vertical_centered(|ui| {
                    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
                    ui.painter().rect_filled(rect.expand(1.0), 8.0, Color32::from_gray(20));
                    egui::Image::new(tex).corner_radius(CornerRadius::same(8)).paint_at(ui, rect);
                });
            }
            ui.add_space(14.0);
            ui.label(RichText::new(title[k]).size(24.0).color(TEXT).strong());
            ui.add_space(4.0);
            ui.label(RichText::new(words[k]).size(15.0).color(DIM));
        }

        fn about(&mut self, ui: &mut egui::Ui) {
            self.heading(ui, self.t("About Painter", "Informazioni su Painter"));
            ui.label(
                RichText::new(self.t(
                    "A painting app for illustration and comics, written in Rust with a GPU canvas. It runs on Windows, macOS, Linux, iPad and Android, and opens and saves Photoshop, Krita, OpenRaster and Clip Studio files.",
                    "Un'app di pittura per illustrazione e fumetti, scritta in Rust con una tela su GPU. Funziona su Windows, macOS, Linux, iPad e Android, e apre e salva file Photoshop, Krita, OpenRaster e Clip Studio.",
                ))
                .size(15.0)
                .color(TEXT),
            );
            ui.add_space(16.0);
            ui.label(RichText::new(self.t("Licence", "Licenza")).size(17.0).color(TEXT).strong());
            ui.label(
                RichText::new(self.t(
                    "Free software, under the MIT or the Apache 2.0 licence, as you prefer: use it, share it and change it, for anything, at no cost. It comes with no warranty.",
                    "Software libero, con licenza MIT o Apache 2.0, a tua scelta: usalo, condividilo e modificalo, per qualsiasi scopo, gratis. Viene fornito senza garanzia.",
                ))
                .color(DIM),
            );
            ui.add_space(16.0);
            ui.label(RichText::new(self.t("Built with", "Realizzato con")).size(17.0).color(TEXT).strong());
            ui.label(RichText::new("egui · wgpu · Little CMS · image · ab_glyph · lyon · zip · flate2 · serde").color(DIM));
            ui.add_space(16.0);
            ui.horizontal(|ui| {
                if ui.link(RichText::new("cr0ma-gh.github.io/painter").color(ACCENT)).clicked() {
                    let _ = std::process::Command::new("explorer").arg("https://cr0ma-gh.github.io/painter/").spawn();
                }
                ui.label(RichText::new("·").color(DIM));
                if ui.link(RichText::new("github.com/cr0ma-gh/painter").color(ACCENT)).clicked() {
                    let _ = std::process::Command::new("explorer").arg("https://github.com/cr0ma-gh/painter").spawn();
                }
            });
        }

        fn options(&mut self, ui: &mut egui::Ui) {
            self.heading(ui, self.t("Where and how", "Dove e come"));
            ui.label(RichText::new(self.t("Install in", "Installa in")).color(DIM));
            ui.horizontal(|ui| {
                let mut text = self.opts.dir.display().to_string();
                if ui.add(egui::TextEdit::singleline(&mut text).desired_width(520.0)).changed() {
                    self.opts.dir = PathBuf::from(text);
                }
                if ui.button(self.t("Browse…", "Sfoglia…")).clicked() {
                    if let Some(dir) = rfd::FileDialog::new().set_directory(&self.opts.dir).pick_folder() {
                        self.opts.dir = if dir.ends_with("Painter") { dir } else { dir.join("Painter") };
                    }
                }
            });
            ui.add_space(14.0);
            let check = |ui: &mut egui::Ui, on: &mut bool, label: &str, hint: &str| {
                ui.checkbox(on, RichText::new(label).size(15.0).color(TEXT));
                ui.label(RichText::new(hint).size(12.5).color(DIM));
                ui.add_space(6.0);
            };
            let (a, b, c, d) = (
                self.t("A shortcut in the Start menu", "Un collegamento nel menu Start"),
                self.t("A shortcut on the desktop", "Un collegamento sul desktop"),
                self.t("Open Painter's files with Painter", "Apri i file di Painter con Painter"),
                self.t("Start Painter when it's done", "Avvia Painter alla fine"),
            );
            let (ha, hb, hc) = (
                self.t("Find Painter by typing its name after pressing Start.", "Trovi Painter scrivendone il nome dopo aver premuto Start."),
                self.t("An icon to start Painter from the desktop.", "Un'icona per avviare Painter dal desktop."),
                self.t(
                    ".painter projects open in Painter with their picture as the thumbnail; PSD, Krita, OpenRaster and Clip Studio files list Painter under Open with.",
                    "I progetti .painter si aprono in Painter con la loro immagine come miniatura; i file PSD, Krita, OpenRaster e Clip Studio elencano Painter in Apri con.",
                ),
            );
            check(ui, &mut self.opts.start_menu, a, ha);
            check(ui, &mut self.opts.desktop, b, hb);
            check(ui, &mut self.opts.file_types, c, hc);
            ui.checkbox(&mut self.launch, RichText::new(d).size(15.0).color(TEXT));
            if !setup::has_payload() {
                ui.add_space(10.0);
                ui.colored_label(
                    Color32::from_rgb(240, 190, 90),
                    self.t(
                        "This installer was built without Painter inside: it can't install.",
                        "Questo installer è stato compilato senza Painter: non può installare.",
                    ),
                );
            }
        }

        fn working(&mut self, ui: &mut egui::Ui) {
            let removing = self.uninstall.is_some();
            self.heading(
                ui,
                if removing {
                    self.t("Removing Painter", "Rimozione di Painter")
                } else {
                    self.t("Installing Painter", "Installazione di Painter")
                },
            );
            let (fraction, step) = {
                let p = self.progress.lock().unwrap();
                (p.fraction, p.step)
            };
            ui.add_space(20.0);
            ui.add(egui::ProgressBar::new(fraction).desired_height(14.0).fill(ACCENT).corner_radius(CornerRadius::same(7)));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new(self.step_name(step)).color(DIM));
            });
        }

        fn done(&mut self, ui: &mut egui::Ui) {
            ui.vertical_centered(|ui| {
                ui.add_space(50.0);
                if let Some(logo) = &self.logo {
                    ui.add(egui::Image::new(logo).fit_to_exact_size(Vec2::splat(110.0)));
                }
                ui.add_space(16.0);
                if self.uninstall.is_some() {
                    ui.label(RichText::new(self.t("Painter was removed.", "Painter è stato rimosso.")).size(24.0).color(TEXT).strong());
                } else {
                    ui.label(RichText::new(self.t("Painter is installed.", "Painter è installato.")).size(24.0).color(TEXT).strong());
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(self.t(
                            "Remove it any time from Settings → Apps. Updates come from Help → Check for Updates.",
                            "Puoi rimuoverlo quando vuoi da Impostazioni → App. Gli aggiornamenti arrivano da Aiuto → Controlla aggiornamenti.",
                        ))
                        .color(DIM),
                    );
                }
            });
        }

        fn failed(&mut self, ui: &mut egui::Ui) {
            self.heading(ui, self.t("Something went wrong", "Qualcosa è andato storto"));
            ui.colored_label(Color32::from_rgb(240, 140, 120), &self.error);
            ui.add_space(8.0);
            ui.label(
                RichText::new(
                    self.t("Nothing else was changed. You can try again.", "Nient'altro è stato modificato. Puoi riprovare."),
                )
                .color(DIM),
            );
        }

        fn remove_confirm(&mut self, ui: &mut egui::Ui) {
            self.heading(ui, self.t("Remove Painter?", "Rimuovere Painter?"));
            if let Some(dir) = &self.uninstall {
                ui.label(RichText::new(format!("{} {}", self.t("From", "Da"), dir.display())).color(DIM));
            }
            ui.add_space(10.0);
            ui.label(
                RichText::new(self.t(
                    "The app, its shortcuts and its file types go. Your pictures and projects stay where they are.",
                    "L'app, i suoi collegamenti e i suoi tipi di file vengono rimossi. Le tue immagini e i tuoi progetti restano dove sono.",
                ))
                .size(15.0)
                .color(TEXT),
            );
            ui.add_space(14.0);
            let label = self.t("Also remove my settings, brushes and workspaces", "Rimuovi anche impostazioni, pennelli e workspace");
            ui.checkbox(&mut self.remove_settings, RichText::new(label).color(TEXT));
        }

        /// The bottom bar: tour dots, Back and Next (or Install, Finish…).
        fn nav(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
            ui.horizontal(|ui| {
                // Where in the tour.
                let tour_at = match self.page {
                    Page::Tour(i) => Some(i),
                    _ => None,
                };
                if self.uninstall.is_none() && matches!(self.page, Page::Welcome | Page::Tour(_) | Page::About) {
                    for i in 0..SLIDES.len() {
                        let (rect, r) = ui.allocate_exact_size(Vec2::splat(14.0), Sense::click());
                        let on = tour_at == Some(i);
                        ui.painter().circle(
                            rect.center(),
                            if on { 5.0 } else { 4.0 },
                            if on { ACCENT } else { SELECTED },
                            Stroke::NONE,
                        );
                        if r.clicked() {
                            self.page = Page::Tour(i);
                            self.slide_since = Instant::now();
                        }
                    }
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let next = self.t("Next", "Avanti");
                    let back = self.t("Back", "Indietro");
                    match self.page {
                        Page::Welcome => {
                            if button(ui, next, true) {
                                self.page = Page::Tour(0);
                                self.slide_since = Instant::now();
                            }
                            if button(ui, self.t("Skip the tour", "Salta il giro"), false) {
                                self.page = Page::Options;
                            }
                        }
                        Page::Tour(i) => {
                            if button(ui, next, true) {
                                self.page = if i + 1 < SLIDES.len() { Page::Tour(i + 1) } else { Page::About };
                                self.slide_since = Instant::now();
                            }
                            if button(ui, back, false) {
                                self.page = if i == 0 { Page::Welcome } else { Page::Tour(i - 1) };
                                self.slide_since = Instant::now();
                            }
                        }
                        Page::About => {
                            if button(ui, next, true) {
                                self.page = Page::Options;
                            }
                            if button(ui, back, false) {
                                self.page = Page::Tour(SLIDES.len() - 1);
                                self.slide_since = Instant::now();
                            }
                        }
                        Page::Options => {
                            let ready = setup::has_payload() && !self.opts.dir.as_os_str().is_empty();
                            let r = ui.add_enabled_ui(ready, |ui| button(ui, self.t("Install", "Installa"), true)).inner;
                            if r {
                                self.start(ctx, false);
                            }
                            if button(ui, back, false) {
                                self.page = Page::About;
                            }
                        }
                        Page::Working => {}
                        Page::Done => {
                            if button(ui, self.t("Close", "Chiudi"), true) {
                                if self.launch && self.uninstall.is_none() {
                                    setup::launch(&self.opts.dir);
                                }
                                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                            }
                            if self.uninstall.is_none() {
                                let label = self.t("Start Painter", "Avvia Painter");
                                ui.checkbox(&mut self.launch, RichText::new(label).color(TEXT));
                            }
                        }
                        Page::Failed => {
                            if button(ui, self.t("Try again", "Riprova"), true) {
                                self.page = if self.uninstall.is_some() { Page::RemoveConfirm } else { Page::Options };
                            }
                            if button(ui, self.t("Close", "Chiudi"), false) {
                                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                            }
                        }
                        Page::RemoveConfirm => {
                            if button(ui, self.t("Remove", "Rimuovi"), true) {
                                self.start(ctx, true);
                            }
                            if button(ui, self.t("Cancel", "Annulla"), false) {
                                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                            }
                        }
                    }
                });
            });
        }

        /// `--screenshot FILE --page N`: that page as a PNG, then quit.
        fn screenshot_tick(&mut self, ctx: &egui::Context) {
            let Some((path, frames)) = &mut self.screenshot else { return };
            *frames += 1;
            if *frames == 4 {
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
            }
            let shot = ctx.input(|i| {
                i.raw.events.iter().find_map(|e| match e {
                    egui::Event::Screenshot { image, .. } => Some(image.clone()),
                    _ => None,
                })
            });
            if let Some(img) = shot {
                let bytes: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
                let _ = image::save_buffer(&*path, &bytes, img.size[0] as u32, img.size[1] as u32, image::ColorType::Rgba8);
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            ctx.request_repaint();
        }
    }

    pub fn run() -> eframe::Result {
        let args: Vec<String> = std::env::args().collect();
        let value = |flag: &str| args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned();
        let removing = args.iter().any(|a| a == "--uninstall");
        // Settings → Apps runs uninstall.exe in Painter's folder: a temp copy does the work.
        let mut uninstall = None;
        if removing {
            let dir = value("--from")
                .map(PathBuf::from)
                .or_else(setup::installed_dir)
                .or_else(|| std::env::current_exe().ok().and_then(|p| p.parent().map(PathBuf::from)));
            if let Some(dir) = &dir {
                if value("--from").is_none() && setup::hand_over_uninstall(dir) {
                    return Ok(());
                }
            }
            uninstall = dir;
        }
        let screenshot = value("--screenshot").map(|p| (PathBuf::from(p), 0));
        let page = match value("--page").and_then(|p| p.parse::<usize>().ok()) {
            Some(0) | None => Page::Welcome,
            Some(n) if n <= SLIDES.len() => Page::Tour(n - 1),
            Some(n) if n == SLIDES.len() + 1 => Page::About,
            Some(_) => Page::Options,
        };
        let it = match value("--lang").as_deref() {
            Some("it") => true,
            Some(_) => false,
            None => setup::italian(),
        };
        let dir = setup::installed_dir().unwrap_or_else(setup::default_dir);
        let app = App {
            it,
            page: if uninstall.is_some() { Page::RemoveConfirm } else { page },
            uninstall,
            opts: Options { dir, start_menu: true, desktop: true, file_types: true, register: true },
            launch: true,
            remove_settings: false,
            slides: Vec::new(),
            logo: None,
            slide_since: Instant::now(),
            progress: Arc::new(Mutex::new(Progress::default())),
            error: String::new(),
            screenshot,
        };
        let icon = image::load_from_memory(include_bytes!("../../../packaging/icons/painter-256.png")).ok().map(|i| {
            let i = i.to_rgba8();
            egui::IconData { width: i.width(), height: i.height(), rgba: i.into_raw() }
        });
        let mut viewport = egui::ViewportBuilder::default()
            .with_title(if it { "Installa Painter" } else { "Install Painter" })
            .with_inner_size([860.0, 620.0])
            .with_resizable(false)
            .with_maximize_button(false);
        if let Some(icon) = icon {
            viewport = viewport.with_icon(icon);
        }
        let options = eframe::NativeOptions { viewport, renderer: eframe::Renderer::Wgpu, ..Default::default() };
        eframe::run_native(
            "Painter Setup",
            options,
            Box::new(move |cc| {
                let mut v = egui::Visuals::dark();
                v.panel_fill = BACKDROP;
                v.window_fill = PANEL;
                v.selection.bg_fill = SELECTED;
                v.hyperlink_color = ACCENT;
                v.widgets.inactive.bg_fill = Color32::from_gray(58);
                cc.egui_ctx.set_visuals(v);
                Ok(Box::new(app))
            }),
        )
    }
}
