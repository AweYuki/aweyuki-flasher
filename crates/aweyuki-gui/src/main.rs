fn main() -> eframe::Result {
    let native_options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Aweyuki Flasher — Obsidian Studio (wgpu)")
            .with_inner_size([1320.0, 840.0])
            .with_min_inner_size([920.0, 620.0]),
        ..Default::default()
    };

    eframe::run_native(
        "Aweyuki Flasher",
        native_options,
        Box::new(|cc| Ok(Box::new(aweyuki_gui::AweyukiApp::new(cc)))),
    )
}
