//! What the host prints to SSH Remote's printer, printed here: each job asks first (print it
//! here, keep it as a PDF in Downloads, or let it go), is fetched as any file into a folder of its
//! own, then printed on this computer's default printer or moved to Downloads.

use super::*;

/// A job being fetched: the host's number for it, its name, and whether it is printed (else kept).
pub(super) struct Fetching {
    job: u32,
    name: String,
    print: bool,
    file: std::path::PathBuf,
}

impl Remote {
    /// A print job the host has: asked about once the pane is shown.
    pub(super) fn print_job(&mut self, id: u32, size: u64, name: String, path: String) {
        self.print_asks.push((id, size, name, path));
    }

    /// The question for the first job waiting.
    pub(in crate::gui) fn print_prompt(&mut self, ctx: &egui::Context) -> Result<(), String> {
        let Some((id, size, name, _)) = self.print_asks.first().cloned() else { return Ok(()) };
        let host = self.host().to_string();
        let mut answer = None;
        let (modal, fit) = crate::gui::modal(ctx, "sshremote-print-job", Vec2::new(440.0, 150.0));
        let modal = modal.show(ctx, |ui| {
            ui.set_width(fit.x);
            ui.label(RichText::new(t!("{} printed", host)).size(18.0).strong());
            ui.label(format!("{name}  ({})", size_text(size)));
            ui.label(RichText::new(t!("It went to the printer SSH Remote there, which prints here.")).weak());
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button(t!("Print here")).clicked() {
                    answer = Some(Some(true));
                }
                if ui.button(t!("Keep as PDF")).on_hover_text(t!("In the Downloads folder")).clicked() {
                    answer = Some(Some(false));
                }
                if ui.button(t!("Let it go")).clicked() {
                    answer = Some(None);
                }
            });
        });
        if modal.should_close() && answer.is_none() {
            answer = Some(None);
        }
        let Some(answer) = answer else { return Ok(()) };
        let (_, _, name, path) = self.print_asks.remove(0);
        match answer {
            None => self.send(ToAgent::Printed(id)),
            Some(print) => {
                let tag = self.next_tag | crate::files::DONE_TAG;
                self.next_tag += 1;
                let dir = crate::files::clipboard_dir(&format!("print-{}-{id}", std::process::id()));
                let file = dir.join(path.rsplit(['/', '\\']).find(|s| !s.is_empty()).unwrap_or("job.pdf"));
                self.fetch_dirs.insert(tag, dir);
                self.print_fetching.insert(tag, Fetching { job: id, name, print, file });
                self.send(ToAgent::Fetch { tag, path })
            }
        }
    }

    /// A fetch ended: if it was a print job's, it is printed or kept, and the host lets it go.
    /// Whether it was one.
    pub(super) fn print_fetched(&mut self, tag: u32) -> Result<bool, String> {
        let Some(f) = self.print_fetching.remove(&tag) else { return Ok(false) };
        self.fetch_dirs.remove(&tag);
        self.send(ToAgent::Printed(f.job))?;
        if !f.file.is_file() {
            self.notify(&t!("{} could not be brought over to print", f.name));
            return Ok(true);
        }
        if f.print {
            let done = self.print_done.0.clone();
            let host = self.host().to_string();
            std::thread::spawn(move || {
                let text = match crate::print::print_here(&f.file, &f.name) {
                    Ok(how) => t!("{} from {}: {}", f.name, host, crate::lang::tr_msg(&how)),
                    Err(e) => t!("{} could not be printed: {}", f.name, crate::lang::tr_msg(&e)),
                };
                let _ = std::fs::remove_file(&f.file);
                let _ = done.send(text);
            });
        } else {
            let keep = || -> std::io::Result<std::path::PathBuf> {
                std::fs::create_dir_all(crate::files::downloads())?;
                let (mut out, to) = crate::files::create_free(&crate::files::downloads(), &f.name)?;
                std::io::copy(&mut std::fs::File::open(&f.file)?, &mut out)?;
                Ok(to)
            };
            let text = match keep() {
                Ok(to) => t!("{} kept in {}", f.name, to.display()),
                Err(e) => t!("{} could not be kept: {}", f.name, e),
            };
            let _ = std::fs::remove_file(&f.file);
            self.notify(&text);
        }
        Ok(true)
    }

    /// What the printing threads came back with.
    pub(super) fn print_results(&mut self) {
        while let Ok(text) = self.print_done.1.try_recv() {
            self.notify(&text);
        }
    }
}

fn size_text(n: u64) -> String {
    match n {
        0..1024 => t!("{} bytes", n),
        1024..1048576 => t!("{} KB", n.div_ceil(1024)),
        _ => t!("{} MB", format!("{:.1}", n as f64 / 1048576.0)),
    }
}
