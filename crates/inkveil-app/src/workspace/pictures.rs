//! Pictures in an open note: inserting them (paste, drop, file picker), showing one large, and
//! the right-click menu (copy, save, size, alignment, delete).

use std::{path::PathBuf, sync::Arc};

use gpui::{
    AnyElement, Context, ExternalPaths, IntoElement, ObjectFit, PathPromptOptions, Pixels, Point,
    RenderImage, Window, anchored, deferred, div, img, point, prelude::*, px,
};
use inkveil_doc::{Align, Block, ImageBlock};
use inkveil_store::BlobId;
use zeroize::Zeroizing;

use super::note_editor::{NoteEditor, NoteEditorEvent};
use crate::{
    assets::Icon,
    images,
    theme::theme,
    ui::{button, icon_button},
};

/// A picture shown large over a blurred, dimmed backdrop.
pub struct Zoom {
    blob: BlobId,
    /// The picture's size in pixels; it is never shown larger than that.
    pixels: (u32, u32),
    image: Option<Arc<RenderImage>>,
    backdrop: Option<Arc<RenderImage>>,
}

/// The right-click menu of a picture.
pub struct PictureMenu {
    index: usize,
    position: Point<Pixels>,
}

/// Display widths offered in the menu.
const SIZES: [(&str, u8); 4] = [
    ("Small", 25),
    ("Medium", 40),
    ("Large", 70),
    ("Full width", 100),
];

impl NoteEditor {
    fn picture(&self, index: usize, cx: &gpui::App) -> Option<ImageBlock> {
        self.editor
            .read(cx)
            .doc()
            .block(index)
            .and_then(Block::as_image)
            .cloned()
    }

    fn report(&self, message: String, cx: &mut Context<Self>) {
        cx.emit(NoteEditorEvent::Problem(message.into()));
    }

    /// Cleans up `bytes`, stores them encrypted and inserts the picture at the caret.
    pub fn insert_picture(&mut self, bytes: Vec<u8>, window: &mut Window, cx: &mut Context<Self>) {
        let storage = self.storage.clone();
        cx.spawn_in(window, async move |this, cx| {
            let prepared = cx
                .background_executor()
                .spawn(async move { images::prepare(&bytes) })
                .await;
            let prepared = match prepared {
                Ok(p) => p,
                Err(error) => {
                    this.update(cx, |this, cx| this.report(error, cx)).ok();
                    return;
                }
            };
            let (width, height) = (prepared.width, prepared.height);
            let stored = storage.put_blob(prepared.bytes).await;
            this.update(cx, |this, cx| match stored {
                Ok(id) => {
                    let image = ImageBlock {
                        blob: id.0,
                        pixel_width: width,
                        pixel_height: height,
                        width_percent: images::DEFAULT_WIDTH_PERCENT,
                        align: Align::Left,
                    };
                    this.editor
                        .update(cx, |e, cx| e.run(cx, |ed| ed.insert_image(image)));
                }
                Err(error) => this.report(format!("Could not store the picture: {error}"), cx),
            })
            .ok();
        })
        .detach();
    }

    /// Inserts picture files (Insert → Image, or files dropped on the note).
    pub fn insert_picture_files(
        &mut self,
        paths: Vec<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        for path in paths {
            let is_picture = path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| images::EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()));
            if !is_picture {
                self.report(
                    format!("{} is not a picture Inkveil can insert.", path.display()),
                    cx,
                );
                continue;
            }
            match std::fs::metadata(&path) {
                Ok(m) if m.len() > images::MAX_INPUT_BYTES as u64 => {
                    self.report("That picture is larger than 50 MB.".into(), cx);
                }
                Ok(_) => match std::fs::read(&path) {
                    Ok(bytes) => self.insert_picture(bytes, window, cx),
                    Err(error) => self.report(format!("Could not open the file: {error}"), cx),
                },
                Err(error) => self.report(format!("Could not open the file: {error}"), cx),
            }
        }
    }

    /// Asks for picture files and inserts them.
    pub fn choose_pictures(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Insert".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = chosen.await {
                this.update_in(cx, |this, window, cx| {
                    this.insert_picture_files(paths, window, cx);
                })
                .ok();
            }
        })
        .detach();
    }

    pub(super) fn on_drop_files(
        &mut self,
        paths: &ExternalPaths,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.insert_picture_files(paths.paths().to_vec(), window, cx);
    }

    // ----- Showing a picture large -------------------------------------------------------------

    pub fn zoom_open(&self) -> bool {
        self.zoom.is_some()
    }

    pub fn open_zoom(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(picture) = self.picture(index, cx) else {
            return;
        };
        let blob = BlobId(picture.blob);
        self.picture_menu = None;
        // Reuse the picture the note already decoded (and a backdrop made earlier), so opening a
        // picture large again and again does not use more memory each time.
        let image = images::ImageCache::ready(blob, cx);
        let backdrop = images::ImageCache::backdrop(blob, cx);
        let complete = image.is_some() && backdrop.is_some();
        self.zoom = Some(Zoom {
            blob,
            pixels: (picture.pixel_width, picture.pixel_height),
            image: image.clone(),
            backdrop: backdrop.clone(),
        });
        cx.notify();
        if complete {
            return;
        }
        let storage = self.storage.clone();
        cx.spawn_in(window, async move |this, cx| {
            let Ok(bytes) = storage.load_blob(blob).await else {
                return;
            };
            let (need_image, need_backdrop) = (image.is_none(), backdrop.is_none());
            let decoded = cx
                .background_executor()
                .spawn(async move {
                    (
                        need_image
                            .then(|| images::decode_for_display(&bytes).ok())
                            .flatten(),
                        need_backdrop
                            .then(|| images::backdrop(&bytes).ok())
                            .flatten(),
                    )
                })
                .await;
            this.update(cx, |this, cx| {
                let (new_image, new_backdrop) = decoded;
                if let Some(image) = &new_image {
                    images::ImageCache::store(blob, image.clone(), cx);
                }
                if let Some(backdrop) = &new_backdrop {
                    images::ImageCache::store_backdrop(blob, backdrop.clone(), cx);
                }
                if let Some(zoom) = this.zoom.as_mut().filter(|z| z.blob == blob) {
                    zoom.image = zoom.image.take().or(new_image);
                    zoom.backdrop = zoom.backdrop.take().or(new_backdrop);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    pub fn close_zoom(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.zoom = None;
        self.editor.update(cx, |e, cx| e.focus(window, cx));
        cx.notify();
    }

    // ----- Copy, save, size, delete ------------------------------------------------------------

    fn copy_picture(&mut self, blob: BlobId, window: &mut Window, cx: &mut Context<Self>) {
        let storage = self.storage.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = match storage.load_blob(blob).await {
                Ok(bytes) => {
                    cx.background_executor()
                        .spawn(async move { images::clipboard_formats(&bytes) })
                        .await
                }
                Err(error) => Err(error.to_string()),
            };
            this.update(cx, |this, cx| match result {
                Ok((png, dib)) => crate::clipboard::write_image(&png, &dib),
                Err(error) => this.report(format!("Could not copy the picture: {error}"), cx),
            })
            .ok();
        })
        .detach();
    }

    /// Saves the picture as a file. This writes an unencrypted copy outside the vault, so it
    /// only happens when asked, to a place the user picks.
    fn save_picture(&mut self, blob: BlobId, window: &mut Window, cx: &mut Context<Self>) {
        let storage = self.storage.clone();
        cx.spawn_in(window, async move |this, cx| {
            let bytes: Zeroizing<Vec<u8>> = match storage.load_blob(blob).await {
                Ok(bytes) => bytes,
                Err(error) => {
                    this.update(cx, |this, cx| {
                        this.report(format!("Could not read the picture: {error}"), cx);
                    })
                    .ok();
                    return;
                }
            };
            let name = format!("picture.{}", images::extension(&bytes));
            let directory = dirs::picture_dir()
                .or_else(dirs::home_dir)
                .unwrap_or_default();
            let Ok(chosen) =
                this.update(cx, |_, cx| cx.prompt_for_new_path(&directory, Some(&name)))
            else {
                return;
            };
            if let Ok(Ok(Some(path))) = chosen.await
                && let Err(error) = std::fs::write(&path, bytes.as_slice())
            {
                this.update(cx, |this, cx| {
                    this.report(format!("Could not save the picture: {error}"), cx);
                })
                .ok();
            }
        })
        .detach();
    }

    fn set_picture_layout(
        &mut self,
        index: usize,
        width: Option<u8>,
        align: Option<Align>,
        cx: &mut Context<Self>,
    ) {
        let Some(picture) = self.picture(index, cx) else {
            return;
        };
        let (width, align) = (
            width.unwrap_or(picture.width_percent),
            align.unwrap_or(picture.align),
        );
        self.editor.update(cx, |e, cx| {
            e.run(cx, |ed| ed.set_image_layout(index, width, align));
        });
    }

    pub(super) fn open_picture_menu(
        &mut self,
        index: usize,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        self.picture_menu = Some(PictureMenu { index, position });
        cx.notify();
    }

    fn close_picture_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.picture_menu = None;
        self.editor.update(cx, |e, cx| e.focus(window, cx));
        cx.notify();
    }

    // ----- Rendering ---------------------------------------------------------------------------

    pub fn render_picture_menu(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let menu = self.picture_menu.as_ref()?;
        let picture = self.picture(menu.index, cx)?;
        let colors = theme(cx);
        let index = menu.index;
        let blob = BlobId(picture.blob);
        let row = |id: &'static str, label: &'static str, checked: bool| {
            div()
                .id(id)
                .flex()
                .items_center()
                .gap_2()
                .h(px(28.))
                .px_2()
                .rounded_sm()
                .cursor_pointer()
                .hover(|d| d.bg(colors.hover))
                .child(
                    div()
                        .w(px(14.))
                        .text_color(colors.accent)
                        .child(if checked { "✓" } else { "" }),
                )
                .child(label)
        };
        let separator = || div().h(px(1.)).my_1().bg(colors.border);
        let mut panel = div()
            .id("picture-menu")
            .occlude()
            .flex()
            .flex_col()
            .p_1()
            .min_w(px(230.))
            .rounded_md()
            .border_1()
            .border_color(colors.border)
            .bg(colors.background)
            .shadow_md()
            .text_size(px(13.))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.picture_menu = None;
                cx.notify();
            }))
            .child(row("pic-open", "Open large", false).on_click(
                cx.listener(move |this, _, window, cx| this.open_zoom(index, window, cx)),
            ))
            .child(row("pic-copy", "Copy image", false).on_click(cx.listener(
                move |this, _, window, cx| {
                    this.copy_picture(blob, window, cx);
                    this.close_picture_menu(window, cx);
                },
            )))
            .child(
                row("pic-save", "Save image as… (unencrypted copy)", false).on_click(cx.listener(
                    move |this, _, window, cx| {
                        this.save_picture(blob, window, cx);
                        this.close_picture_menu(window, cx);
                    },
                )),
            )
            .child(separator());
        for (i, (label, width)) in SIZES.into_iter().enumerate() {
            panel = panel.child(
                row(
                    ["pic-s", "pic-m", "pic-l", "pic-f"][i],
                    label,
                    picture.width_percent == width,
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.set_picture_layout(index, Some(width), None, cx);
                    this.close_picture_menu(window, cx);
                })),
            );
        }
        panel = panel.child(separator());
        for (id, label, align) in [
            ("pic-left", "Align left", Align::Left),
            ("pic-center", "Center", Align::Center),
            ("pic-right", "Align right", Align::Right),
        ] {
            panel = panel.child(row(id, label, picture.align == align).on_click(cx.listener(
                move |this, _, window, cx| {
                    this.set_picture_layout(index, None, Some(align), cx);
                    this.close_picture_menu(window, cx);
                },
            )));
        }
        panel = panel.child(separator()).child(
            row("pic-delete", "Delete image", false)
                .text_color(colors.error)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.editor
                        .update(cx, |e, cx| e.run(cx, |ed| ed.remove_object(index)));
                    this.close_picture_menu(window, cx);
                })),
        );
        Some(
            deferred(
                anchored()
                    .position(menu.position)
                    .snap_to_window()
                    .child(panel),
            )
            .with_priority(2)
            .into_any_element(),
        )
    }

    /// The large view: the picture over a stretched, blurred copy of itself, dimmed.
    pub fn render_zoom(&self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let zoom = self.zoom.as_ref()?;
        let colors = theme(cx);
        let viewport = window.viewport_size();
        let blob = zoom.blob;
        let toolbar = div()
            .absolute()
            .top(px(16.))
            .right(px(16.))
            .flex()
            .gap_2()
            .child(button("zoom-copy", "Copy", false, cx).on_click(
                cx.listener(move |this, _, window, cx| this.copy_picture(blob, window, cx)),
            ))
            .child(button("zoom-save", "Save as…", false, cx).on_click(
                cx.listener(move |this, _, window, cx| this.save_picture(blob, window, cx)),
            ))
            .child(
                icon_button("zoom-close", Icon::X, false, "Close (Escape)", cx)
                    .size(px(32.))
                    .bg(colors.surface)
                    .on_click(cx.listener(|this, _, window, cx| this.close_zoom(window, cx))),
            );
        // Fit within most of the window, but never enlarge a small picture past its own size.
        let scale = window.scale_factor().max(1.);
        let (pw, ph) = (zoom.pixels.0 as f32 / scale, zoom.pixels.1 as f32 / scale);
        let fit = (viewport.width.as_f32() * 0.88 / pw)
            .min(viewport.height.as_f32() * 0.84 / ph)
            .min(1.);
        let picture: AnyElement = match &zoom.image {
            Some(image) => img(image.clone())
                .w(px(pw * fit))
                .h(px(ph * fit))
                .object_fit(ObjectFit::Contain)
                .rounded_lg()
                .shadow_lg()
                .into_any_element(),
            None => div()
                .text_color(gpui::white())
                .child("Opening…")
                .into_any_element(),
        };
        Some(
            deferred(
                anchored().position(point(px(0.), px(0.))).child(
                    div()
                        .id("zoom")
                        .occlude()
                        .relative()
                        .w(viewport.width)
                        .h(viewport.height)
                        .bg(gpui::rgb(0x000000))
                        .children(zoom.backdrop.clone().map(|backdrop| {
                            img(backdrop)
                                .absolute()
                                .inset_0()
                                .size_full()
                                .object_fit(ObjectFit::Cover)
                                .opacity(0.9)
                        }))
                        .child(div().absolute().inset_0().bg(gpui::rgba(0x0000_00a6)))
                        .child(
                            div()
                                .absolute()
                                .inset_0()
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(div().id("zoom-picture").occlude().child(picture)),
                        )
                        .child(toolbar)
                        .on_click(cx.listener(|this, _, window, cx| this.close_zoom(window, cx))),
                ),
            )
            .with_priority(4)
            .into_any_element(),
        )
    }
}
