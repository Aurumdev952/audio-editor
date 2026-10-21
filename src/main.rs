mod app;
mod audio;
mod export;
mod model;
mod translate;

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1500.0, 920.0]).with_title("Audio Editor"),
        ..Default::default()
    };
    eframe::run_native("Audio Editor", options, Box::new(move |cc| Ok(Box::new(app::App::new(cc, &args)))))
}
