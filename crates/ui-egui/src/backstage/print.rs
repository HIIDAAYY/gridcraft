use std::sync::Arc;

use egui::Color32;
use gridcraft_engine::cmd::print::{PreviewOp, PreviewPage};

use super::{
    Align2, Backstage, Icon, Json, Outcome, Rect, Sense, SheetApp, Stroke, StrokeKind, Tokens, Ui, dim, json, pos2, square_button, theme, vec2,
};

/// Builds the print scope and fit-to-page parameters, caches `file.printPreview` per document revision/options, and draws page thumbnails with navigation plus the print settings and Print action.
pub(super) fn print(app: &mut SheetApp, ui: &mut Ui, bs: &mut Backstage, out: &mut Outcome) {
    let t = Tokens::get(ui.ctx());
    let lang = app.ui.language;
    let Some((uid, revision)) = app.session.active().map(|d| (d.uid, d.revision)) else {
        dim(ui, lang.tr("No workbook is open."));
        return;
    };
    let mut params = json!({"sheets": if bs.print_all { "all" } else { "active" }});
    if bs.fit_to_page {
        params["fitToPage"] = json!(true);
    }
    let key = (uid, revision, bs.print_all, bs.fit_to_page);
    if bs.preview.as_ref().is_none_or(|(k, _)| *k != key) {
        let preview = app.session.run("file.printPreview", params.clone()).unwrap_or(Json::Null);
        bs.preview = Some((key, preview));
    }
    let pages: Vec<Json> = bs.preview.as_ref().and_then(|(_, p)| p.get("pageList")).and_then(Json::as_array).cloned().unwrap_or_default();
    ui.horizontal_top(|ui| {
        ui.vertical(|ui| {
            ui.set_width(250.0);
            if let Some(discover) = &app.services.printers {
                crate::printing::show(&mut app.printing, ui, discover, |label| lang.tr(label).to_owned());
            }
            let can_print = app.services.print_native.is_none() || app.printing.ready();
            if ui.add_enabled_ui(can_print, |ui| square_button(ui, Icon::Print, lang.tr("Print"))).inner.clicked()
                && app.commit_edit(0, 0, false, false)
            {
                app.print_now(params.clone());
                out.close = app.services.print_native.is_none();
            }
            ui.add_space(16.0);
            let scope = if bs.print_all { "Print Entire Workbook" } else { "Print Active Sheets" };
            craft_ui::ComboBox::from_id_salt("backstage_print_scope")
                .width(230.0)
                .selected_text(lang.tr(scope))
                .show_ui(ui, |ui| {
                    for (all, label) in [(false, "Print Active Sheets"), (true, "Print Entire Workbook")] {
                        if ui.selectable_label(bs.print_all == all, lang.tr(label)).clicked() {
                            bs.print_all = all;
                            bs.preview_page = 0;
                        }
                    }
                })
                .response
                .accessible_name(lang.tr("Print What"));
            ui.add_space(8.0);
            if ui.checkbox(&mut bs.fit_to_page, lang.tr("Fit Sheet on One Page")).changed() {
                bs.preview_page = 0;
            }
            ui.add_space(8.0);
            if ui.link(lang.tr("Page Setup…")).clicked() {
                app.open_dialog("pageSetup", json!({}));
            }
        });
        ui.add_space(24.0);
        ui.vertical(|ui| {
            if pages.is_empty() {
                dim(ui, lang.tr("Nothing to print."));
                return;
            }
            let last = pages.len().saturating_sub(1);
            bs.preview_page = bs.preview_page.min(last);
            let Some(page) = pages.get(bs.preview_page) else { return };
            // Paper proportions from the preview (points); hostile sizes fall back to Letter.
            let w = page.get("width").and_then(Json::as_f64).filter(|w| w.is_finite() && *w > 1.0).unwrap_or(612.0) as f32;
            let h = page.get("height").and_then(Json::as_f64).filter(|h| h.is_finite() && *h > 1.0).unwrap_or(792.0) as f32;
            let scale = (520.0 / h).min(440.0 / w);
            let (rect, _) = ui.allocate_exact_size(vec2(w * scale, h * scale), Sense::hover());
            let p = ui.painter();
            p.rect_filled(rect.translate(vec2(2.0, 3.0)), 2.0, t.shadow);
            p.rect_filled(rect, 2.0, t.grid_bg);
            p.rect_stroke(rect, 2.0, Stroke::new(1.0, t.ribbon_border), StrokeKind::Inside);
            let page_key = (uid, revision, bs.print_all, bs.fit_to_page, bs.preview_page);
            if bs.drawn.as_ref().is_none_or(|(k, _)| *k != page_key)
                && let Some(d) = app.session.active()
            {
                let path = d.path.clone().unwrap_or_default();
                let drawn = gridcraft_engine::cmd::print::preview_page(&d.wb, &params, bs.preview_page.saturating_add(1), &d.display_title(), &path)
                    .ok()
                    .flatten()
                    .map(Arc::new);
                bs.drawn = Some((page_key, drawn));
            }
            match bs.drawn.as_ref().and_then(|(_, d)| d.clone()) {
                Some(drawn) => paint_drawn(ui, rect, &drawn, page_key),
                None => {
                    if let Some(d) = app.session.active() {
                        paint_page(p, rect, w, page, &d.wb, &t);
                    }
                }
            }
            let sheet = page.get("sheet").and_then(Json::as_str).unwrap_or("");
            let range = page.get("range").and_then(Json::as_str).unwrap_or("");
            ui.add_space(4.0);
            dim(ui, &format!("{sheet} · {range}"));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.add_enabled(bs.preview_page > 0, egui::Button::new("◀")).on_hover_text(lang.tr("Previous page")).clicked() {
                    bs.preview_page = bs.preview_page.saturating_sub(1);
                }
                let text = lang
                    .tr("Page {page} of {pages}")
                    .replace("{page}", &(bs.preview_page + 1).to_string())
                    .replace("{pages}", &pages.len().to_string());
                ui.label(text);
                if ui.add_enabled(bs.preview_page < last, egui::Button::new("▶")).on_hover_text(lang.tr("Next page")).clicked() {
                    bs.preview_page = (bs.preview_page + 1).min(last);
                }
            });
        });
    });
}

/// Which preview page is drawn: (document, revision, whole workbook, fit to page, page).
pub(super) type PageKey = (u64, u64, bool, bool, usize);

fn color(c: [u8; 3]) -> Color32 {
    Color32::from_rgb(c[0], c[1], c[2])
}

/// Draws a printed page from what the PDF draws on it (fills, borders, fonts, pictures, charts),
/// scaled into `rect`.
fn paint_drawn(ui: &Ui, rect: Rect, page: &PreviewPage, key: PageKey) {
    let k = rect.width() / page.width.max(1.0);
    let at = |x: f32, y: f32| pos2(rect.left() + x * k, rect.top() + y * k);
    let area = |r: &gridcraft_engine::cmd::print::PreviewRect| Rect::from_min_size(at(r.x, r.y), vec2(r.width * k, r.height * k));
    let line = |w: f32| (w * k).max(0.6);
    // The pictures' textures, made once per drawn page.
    let id = egui::Id::new(("gridcraft.print_preview_images", key));
    let textures = ui.ctx().data(|d| d.get_temp::<Arc<Vec<egui::TextureHandle>>>(id)).unwrap_or_else(|| {
        let made: Vec<egui::TextureHandle> = page
            .ops
            .iter()
            .filter_map(|op| match op {
                PreviewOp::Image { rgba, size, .. } => {
                    let [w, h] = *size;
                    let n = (w as usize).saturating_mul(h as usize).saturating_mul(4);
                    let pixels = rgba.get(..n)?;
                    let image = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], pixels);
                    Some(ui.ctx().load_texture("print-preview-picture", image, egui::TextureOptions::LINEAR))
                }
                _ => None,
            })
            .collect();
        let made = Arc::new(made);
        ui.ctx().data_mut(|d| d.insert_temp(id, made.clone()));
        made
    });
    let mut pictures = textures.iter();
    let mut clip = rect;
    let mut saved = Vec::new();
    for op in &page.ops {
        let p = ui.painter().with_clip_rect(clip.intersect(ui.clip_rect()));
        match op {
            PreviewOp::Save => saved.push(clip),
            PreviewOp::Restore => clip = saved.pop().unwrap_or(rect),
            PreviewOp::Clip(r) => clip = clip.intersect(area(r)),
            PreviewOp::FillRect(r, c) => {
                p.rect_filled(area(r), 0.0, color(*c));
            }
            PreviewOp::StrokeRect(r, w, c) => {
                p.rect_stroke(area(r), 0.0, Stroke::new(line(*w), color(*c)), StrokeKind::Middle);
            }
            PreviewOp::Polyline { points, width, color: c, dashed } => {
                let pts: Vec<egui::Pos2> = points.iter().map(|q| at(q[0], q[1])).collect();
                let stroke = Stroke::new(line(*width), color(*c));
                if *dashed {
                    p.extend(egui::Shape::dashed_line(&pts, stroke, 3.0, 2.0));
                } else {
                    p.add(egui::Shape::line(pts, stroke));
                }
            }
            PreviewOp::Polygon { points, fill, stroke } => {
                let pts: Vec<egui::Pos2> = points.iter().map(|q| at(q[0], q[1])).collect();
                let outline = stroke.map_or(Stroke::NONE, |(c, w)| Stroke::new(line(w), color(c)));
                p.add(egui::Shape::convex_polygon(pts, fill.map_or(Color32::TRANSPARENT, color), outline));
            }
            PreviewOp::Circle { center, radius, fill, stroke } => {
                let c = at(center.x, center.y);
                if let Some(f) = fill {
                    p.circle_filled(c, radius * k, color(*f));
                }
                if let Some((sc, w)) = stroke {
                    p.circle_stroke(c, radius * k, Stroke::new(line(*w), color(*sc)));
                }
            }
            PreviewOp::Text { origin, size, font, color: c, text, angle } => {
                let px = size * k;
                if px < 1.0 {
                    continue;
                }
                let face = if font.is_bold() { theme::ui_bold(px) } else { theme::ui_font(px) };
                let galley = p.layout_no_wrap(text.clone(), face, color(*c));
                // The PDF places the baseline; the galley is placed by its top.
                let top = at(origin.x, origin.y) - vec2(0.0, px * 0.8);
                p.add(egui::epaint::TextShape::new(top, galley, color(*c)).with_angle(if angle.is_finite() { *angle } else { 0.0 }));
            }
            PreviewOp::Image { rect: r, .. } => {
                if let Some(t) = pictures.next() {
                    p.image(t.id(), area(r), Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                }
            }
        }
    }
}

/// Most rows and columns one preview page draws (a page holds far fewer at any readable scale).
const PREVIEW_ROWS: u32 = 200;

const PREVIEW_COLS: u32 = 60;

/// Draws one printed page in miniature: the cells' displayed text at the page's print scale,
/// inside the sheet's margins. `paper_w` is the page width in points.
fn paint_page(p: &egui::Painter, rect: Rect, paper_w: f32, page: &Json, wb: &gridcraft_engine::model::Workbook, t: &Tokens) {
    let Some(range) = page.get("range").and_then(Json::as_str).and_then(gridcraft_engine::core::RangeRef::parse) else { return };
    let Some(sh) = page.get("sheet").and_then(Json::as_str).and_then(|n| wb.sheet_index(n)).and_then(|i| wb.sheets.get(i)) else { return };
    // Preview pixels per point, and per sheet pixel (sheet geometry is 96 dpi: 0.75 pt per pixel).
    let per_pt = rect.width() / paper_w;
    let print_scale =
        page.get("scale").and_then(Json::as_f64).filter(|s| s.is_finite() && *s > 0.0).map_or(1.0, |s| (s / 100.0) as f32).clamp(0.1, 4.0);
    let per_px = per_pt * 0.75 * print_scale;
    let margin =
        |i: usize, default: f32| sh.print.margins.get(i).copied().filter(|m| m.is_finite()).unwrap_or(default).clamp(0.0, 3.0) * 72.0 * per_pt;
    let body = Rect::from_min_max(
        pos2(rect.left() + margin(0, 0.7), rect.top() + margin(2, 0.75)),
        pos2(rect.right() - margin(1, 0.7), rect.bottom() - margin(3, 0.75)),
    );
    if !body.is_positive() {
        return;
    }
    let font = theme::ui_font((11.0 * per_pt * print_scale).clamp(3.0, 14.0));
    let last_row = range.end.row.min(range.start.row.saturating_add(PREVIEW_ROWS));
    let last_col = range.end.col.min(range.start.col.saturating_add(PREVIEW_COLS));
    let mut y = body.top();
    for row in range.start.row..=last_row {
        let h = sh.row_height(row) * per_px;
        if h <= 0.0 {
            continue;
        }
        if y >= body.bottom() {
            break;
        }
        let mut x = body.left();
        for col in range.start.col..=last_col {
            let w = sh.col_width(col) * per_px;
            if w <= 0.0 {
                continue;
            }
            if x >= body.right() {
                break;
            }
            let cell = gridcraft_engine::core::CellRef::new(row, col);
            let text = gridcraft_engine::display::cell_text(wb, sh, cell);
            if !text.is_empty() {
                let numeric = sh.value(cell).as_f64().is_some();
                // Text runs on over empty neighbours, as on the sheet; numbers stay in their cell.
                let mut span = w;
                if !numeric {
                    let mut next = col.saturating_add(1);
                    while next <= last_col && x + span < body.right() && sh.value(gridcraft_engine::core::CellRef::new(row, next)).is_empty() {
                        span += sh.col_width(next) * per_px;
                        next = next.saturating_add(1);
                    }
                }
                let clip = Rect::from_min_size(pos2(x, y), vec2(span, h)).intersect(body);
                let (anchor, at) =
                    if numeric { (Align2::RIGHT_CENTER, pos2(x + w - 1.0, y + h / 2.0)) } else { (Align2::LEFT_CENTER, pos2(x + 1.0, y + h / 2.0)) };
                p.with_clip_rect(clip).text(at, anchor, text, font.clone(), t.cell_text);
            }
            x += w;
        }
        y += h;
    }
}
