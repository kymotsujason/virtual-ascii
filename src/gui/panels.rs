use eframe::egui;

use crate::config;

use super::app::VirtualAsciiApp;
use super::state::ViewMode;
use super::v4l2_manager;

pub fn settings_panel(ctx: &egui::Context, app: &mut VirtualAsciiApp) {
    egui::SidePanel::left("settings_panel")
        .resizable(true)
        .default_width(280.0)
        .show(ctx, |ui| {
            ui.heading("Settings");
            ui.separator();

            egui::ScrollArea::vertical().show(ui, |ui| {
                camera_section(ui, app);
                ui.add_space(4.0);
                appearance_section(ui, app);
                ui.add_space(4.0);
                v4l2_section(ui, app);
                ui.add_space(4.0);
                pipeline_section(ui, app);
            });
        });
}

fn camera_section(ui: &mut egui::Ui, app: &mut VirtualAsciiApp) {
    egui::CollapsingHeader::new("Camera")
        .default_open(true)
        .show(ui, |ui| {
            // Camera device dropdown
            let current_label = app
                .state
                .detected_cameras
                .iter()
                .find(|c| c.index == app.state.camera_index)
                .map(|c| format!("/dev/video{} ({})", c.index, c.name))
                .unwrap_or_else(|| format!("/dev/video{}", app.state.camera_index));

            let mut changed = false;
            egui::ComboBox::from_label("Camera")
                .selected_text(&current_label)
                .show_ui(ui, |ui| {
                    for cam in &app.state.detected_cameras {
                        let label = format!("/dev/video{} ({})", cam.index, cam.name);
                        if ui
                            .selectable_value(&mut app.state.camera_index, cam.index, &label)
                            .changed()
                        {
                            changed = true;
                        }
                    }
                });

            if changed {
                app.state.refresh_resolutions();
                if app.state.pipeline_running {
                    app.change_camera(app.state.camera_index);
                }
            }

            if ui.small_button("Refresh").clicked() {
                app.state.refresh_cameras();
                app.state.refresh_resolutions();
            }

            // Resolution dropdown
            let res_text = app.state.available_resolutions[app.state.resolution_index].clone();
            let prev_res_index = app.state.resolution_index;
            egui::ComboBox::from_label("Resolution")
                .selected_text(&res_text)
                .show_ui(ui, |ui| {
                    for (i, res) in app.state.available_resolutions.clone().iter().enumerate() {
                        ui.selectable_value(&mut app.state.resolution_index, i, res);
                    }
                });

            if app.state.resolution_index != prev_res_index {
                app.state.refresh_max_fps();
            }

            // FPS slider
            let mut fps = app.state.fps as i32;
            if ui
                .add(egui::Slider::new(&mut fps, 1..=app.state.max_fps as i32).text("FPS"))
                .changed()
            {
                app.state.fps = fps as u32;
                app.state.capture_dirty = true;
                app.state.last_change_time = Some(std::time::Instant::now());
            }
        });
}

fn appearance_section(ui: &mut egui::Ui, app: &mut VirtualAsciiApp) {
    egui::CollapsingHeader::new("Appearance")
        .default_open(true)
        .show(ui, |ui| {
            // Theme dropdown
            let themes = config::theme_names();
            let prev_theme = app.state.theme_name.clone();
            egui::ComboBox::from_label("Theme")
                .selected_text(&app.state.theme_name)
                .show_ui(ui, |ui| {
                    for &name in themes {
                        ui.selectable_value(&mut app.state.theme_name, name.to_string(), name);
                    }
                });

            if app.state.theme_name != prev_theme {
                // Update colors to match new theme defaults
                if let Some(theme) = config::ColorTheme::from_name(&app.state.theme_name) {
                    app.state.fg_color = [theme.fg.r, theme.fg.g, theme.fg.b];
                    app.state.bg_color = [theme.bg.r, theme.bg.g, theme.bg.b];
                }
                app.state.render_dirty = true;
                app.state.last_change_time = Some(std::time::Instant::now());
            }

            // Definition slider
            let mut def = app.state.definition as i32;
            if ui
                .add(egui::Slider::new(&mut def, 1..=10).text("Definition"))
                .changed()
            {
                app.state.definition = def as u8;
                app.state.render_dirty = true;
                app.state.last_change_time = Some(std::time::Instant::now());
            }

            // FG color picker
            if ui
                .horizontal(|ui| {
                    ui.label("FG Color");
                    ui.color_edit_button_srgb(&mut app.state.fg_color)
                })
                .inner
                .changed()
            {
                app.state.render_dirty = true;
                app.state.last_change_time = Some(std::time::Instant::now());
            }

            // BG color picker
            if ui
                .horizontal(|ui| {
                    ui.label("BG Color");
                    ui.color_edit_button_srgb(&mut app.state.bg_color)
                })
                .inner
                .changed()
            {
                app.state.render_dirty = true;
                app.state.last_change_time = Some(std::time::Instant::now());
            }

            // Brightness curve dropdown
            let curves = ["linear", "exponential", "sigmoid"];
            let prev_curve = app.state.brightness_curve_name.clone();
            egui::ComboBox::from_label("Brightness Curve")
                .selected_text(&app.state.brightness_curve_name)
                .show_ui(ui, |ui| {
                    for &name in &curves {
                        ui.selectable_value(
                            &mut app.state.brightness_curve_name,
                            name.to_string(),
                            name,
                        );
                    }
                });

            if app.state.brightness_curve_name != prev_curve {
                app.state.render_dirty = true;
                app.state.last_change_time = Some(std::time::Instant::now());
            }

            // Invert checkbox
            if ui
                .checkbox(&mut app.state.invert, "Invert brightness")
                .changed()
            {
                app.state.render_dirty = true;
                app.state.last_change_time = Some(std::time::Instant::now());
            }
        });
}

fn v4l2_section(ui: &mut egui::Ui, app: &mut VirtualAsciiApp) {
    egui::CollapsingHeader::new("v4l2loopback")
        .default_open(true)
        .show(ui, |ui| {
            // Status
            if app.state.v4l2loopback_loaded {
                ui.colored_label(egui::Color32::from_rgb(0, 200, 0), "Module: loaded");
            } else {
                ui.colored_label(egui::Color32::from_rgb(200, 0, 0), "Module: not loaded");
            }

            // Device path
            ui.horizontal(|ui| {
                ui.label("Device:");
                ui.label(&app.state.output_device);
            });

            // Load/Unload buttons
            ui.horizontal(|ui| {
                if !app.state.v4l2loopback_loaded {
                    if ui.button("Load Module").clicked() {
                        // Parse video_nr from output_device path
                        let video_nr = app
                            .state
                            .output_device
                            .trim_start_matches("/dev/video")
                            .parse::<u32>()
                            .unwrap_or(20);
                        v4l2_manager::load_v4l2loopback(
                            video_nr,
                            "Virtual ASCII",
                            app.v4l2_op_result.clone(),
                        );
                        app.state.status_message = "Loading v4l2loopback...".into();
                    }
                } else {
                    if ui.button("Unload Module").clicked() {
                        v4l2_manager::unload_v4l2loopback(app.v4l2_op_result.clone());
                        app.state.status_message = "Unloading v4l2loopback...".into();
                    }
                }

                if ui.small_button("Refresh").clicked() {
                    app.state.v4l2loopback_loaded = v4l2_manager::is_v4l2loopback_loaded();
                }
            });
        });
}

fn pipeline_section(ui: &mut egui::Ui, app: &mut VirtualAsciiApp) {
    egui::CollapsingHeader::new("Pipeline Control")
        .default_open(true)
        .show(ui, |ui| {
            if !app.state.pipeline_running {
                ui.horizontal(|ui| {
                    if ui.button("Start Camera").clicked() {
                        match app.start_pipeline() {
                            Ok(()) => {}
                            Err(e) => app.state.status_message = format!("Error: {}", e),
                        }
                    }
                    let can_start_all = app.state.v4l2loopback_loaded;
                    if ui.add_enabled(can_start_all, egui::Button::new("Start All")).clicked() {
                        match app.start_pipeline() {
                            Ok(()) => {
                                match app.start_v4l2_output() {
                                    Ok(()) => {}
                                    Err(e) => app.state.status_message = format!("Error starting virtual camera: {}", e),
                                }
                            }
                            Err(e) => app.state.status_message = format!("Error: {}", e),
                        }
                    }
                });
                if !app.state.v4l2loopback_loaded {
                    ui.label(egui::RichText::new("Load v4l2loopback module for virtual camera").small());
                }
            } else {
                // Virtual camera button
                if !app.state.v4l2_output_active {
                    let enabled = app.state.v4l2loopback_loaded;
                    if ui
                        .add_enabled(enabled, egui::Button::new("Start Virtual Camera"))
                        .clicked()
                    {
                        match app.start_v4l2_output() {
                            Ok(()) => {}
                            Err(e) => app.state.status_message = format!("Error: {}", e),
                        }
                    }
                    if !enabled {
                        ui.label(egui::RichText::new("Load v4l2loopback module first").small());
                    }
                } else {
                    if ui.button("Stop Virtual Camera").clicked() {
                        app.stop_v4l2_output();
                    }
                }

                ui.add_space(8.0);

                if ui.button("Stop All").clicked() {
                    app.stop_pipeline();
                }
            }

            // Camera conflict warning
            if let Some(ref conflict) = app.state.camera_conflict {
                ui.colored_label(
                    egui::Color32::from_rgb(255, 200, 0),
                    format!("Warning: {}", conflict),
                );
            }
        });
}

pub fn preview_panel(ctx: &egui::Context, app: &mut VirtualAsciiApp) {
    egui::CentralPanel::default().show(ctx, |ui| {
        if !app.state.pipeline_running
            && app.raw_preview_texture.is_none()
            && app.rendered_preview_texture.is_none()
        {
            ui.centered_and_justified(|ui| {
                ui.heading("Click 'Start Camera' to begin");
            });
            return;
        }

        // View mode selector
        ui.horizontal(|ui| {
            ui.selectable_value(&mut app.state.view_mode, ViewMode::SideBySide, "Side by Side");
            ui.selectable_value(&mut app.state.view_mode, ViewMode::RawOnly, "Raw Camera");
            ui.selectable_value(&mut app.state.view_mode, ViewMode::AsciiOnly, "ASCII Output");
        });
        ui.separator();

        let available = ui.available_size();

        match app.state.view_mode {
            ViewMode::SideBySide => {
                let half_width = (available.x - 12.0) / 2.0;
                ui.columns(2, |cols| {
                    cols[0].vertical_centered(|ui| {
                        ui.label("Raw Camera");
                        if let Some(ref tex) = app.raw_preview_texture {
                            let tex_size = tex.size_vec2();
                            let scale = (half_width / tex_size.x).min(available.y / tex_size.y) * 0.95;
                            let display_size = egui::vec2(tex_size.x * scale, tex_size.y * scale);
                            ui.image(egui::load::SizedTexture::new(tex.id(), display_size));
                        } else {
                            ui.label("No frames yet");
                        }
                    });

                    cols[1].vertical_centered(|ui| {
                        ui.label("ASCII Output");
                        if let Some(ref tex) = app.rendered_preview_texture {
                            let tex_size = tex.size_vec2();
                            let scale = (half_width / tex_size.x).min(available.y / tex_size.y) * 0.95;
                            let display_size = egui::vec2(tex_size.x * scale, tex_size.y * scale);
                            ui.image(egui::load::SizedTexture::new(tex.id(), display_size));
                        } else {
                            ui.label("No frames yet");
                        }
                    });
                });
            }
            ViewMode::RawOnly => {
                ui.vertical_centered(|ui| {
                    ui.label("Raw Camera");
                    if let Some(ref tex) = app.raw_preview_texture {
                        let tex_size = tex.size_vec2();
                        let scale = (available.x / tex_size.x).min(available.y / tex_size.y) * 0.95;
                        let display_size = egui::vec2(tex_size.x * scale, tex_size.y * scale);
                        ui.image(egui::load::SizedTexture::new(tex.id(), display_size));
                    } else {
                        ui.label("No frames yet");
                    }
                });
            }
            ViewMode::AsciiOnly => {
                ui.vertical_centered(|ui| {
                    ui.label("ASCII Output");
                    if let Some(ref tex) = app.rendered_preview_texture {
                        let tex_size = tex.size_vec2();
                        let scale = (available.x / tex_size.x).min(available.y / tex_size.y) * 0.95;
                        let display_size = egui::vec2(tex_size.x * scale, tex_size.y * scale);
                        ui.image(egui::load::SizedTexture::new(tex.id(), display_size));
                    } else {
                        ui.label("No frames yet");
                    }
                });
            }
        }
    });
}

pub fn status_bar(ctx: &egui::Context, app: &mut VirtualAsciiApp) {
    egui::TopBottomPanel::bottom("status_bar").show(ctx, |ui| {
        ui.horizontal(|ui| {
            let status = if app.state.pipeline_running {
                if app.state.v4l2_output_active {
                    "Camera: Active | Virtual Camera: Active"
                } else {
                    "Camera: Active | Virtual Camera: Off"
                }
            } else {
                "Camera: Off"
            };
            ui.label(status);
            ui.separator();
            ui.label(&app.state.status_message);
        });
    });
}
