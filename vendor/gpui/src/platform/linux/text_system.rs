use crate::{
    Bounds, DevicePixels, Font, FontFeatures, FontId, FontMetrics, FontRun, FontStyle, FontWeight,
    GlyphId, LineLayout, Pixels, PlatformTextSystem, Point, RenderGlyphParams, SUBPIXEL_VARIANTS_X,
    SUBPIXEL_VARIANTS_Y, ShapedGlyph, ShapedRun, SharedString, Size, point, size,
};
use anyhow::{Context as _, Ok, Result};
use collections::HashMap;
use cosmic_text::{
    Attrs, AttrsList, CacheKey, Family, Font as CosmicTextFont, FontFeatures as CosmicFontFeatures,
    FontSystem, ShapeBuffer, ShapeLine, SwashCache,
};
use font_kit::{
    canvas::{Canvas, Format as CanvasFormat, RasterizationOptions},
    hinting::HintingOptions,
    loaders::freetype::{Font as FreetypeFont, NativeFont as FreetypeNativeFont},
};
use itertools::Itertools;
use parking_lot::RwLock;
use pathfinder_geometry::{
    rect::{RectF, RectI},
    transform2d::Transform2F,
    vector::{Vector2F, Vector2I},
};
use smallvec::SmallVec;
use std::{
    borrow::Cow,
    cell::RefCell,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

unsafe extern "C" {
    fn FT_Set_Var_Design_Coordinates(
        face: FreetypeNativeFont,
        num_coords: libc::c_uint,
        coords: *const libc::c_long,
    ) -> libc::c_int;
    fn FT_Done_Face(face: FreetypeNativeFont) -> libc::c_int;
}

static NEXT_TEXT_SYSTEM_ID: AtomicU64 = AtomicU64::new(1);

thread_local! {
    /// Per-thread FreeType faces bound to the current thread's `font_kit` `FREETYPE_LIBRARY`,
    /// keyed by `(CosmicTextSystem instance id, FontId)` so variable-font `wght` coordinates
    /// are never shared across threads or outlive their owning FreeType library.
    static VARIABLE_FREETYPE_FONTS: RefCell<HashMap<(u64, FontId), FreetypeFont>> =
        RefCell::new(HashMap::default());
}

pub(crate) struct CosmicTextSystem(RwLock<CosmicTextSystemState>);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct FontKey {
    family: SharedString,
    features: FontFeatures,
}

impl FontKey {
    fn new(family: SharedString, features: FontFeatures) -> Self {
        Self { family, features }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct VarGlyphCacheKey {
    font_id: FontId,
    glyph_id: u32,
    font_size_bits: u32,
    subpixel_x: u8,
    subpixel_y: u8,
}

#[derive(Clone)]
struct VarGlyphBitmap {
    origin_x: i32,
    origin_y: i32,
    width: i32,
    height: i32,
    data: Vec<u8>,
}

#[derive(Clone)]
struct VariableFontInstance {
    face_index: u32,
    design_coords: SmallVec<[f32; 4]>,
    norm_coords: SmallVec<[i16; 4]>,
}

struct VariableWeightAxis {
    default_coords: SmallVec<[f32; 4]>,
    wght_index: usize,
    wght_min: f32,
    wght_max: f32,
}

struct CosmicTextSystemState {
    instance_id: u64,
    swash_cache: SwashCache,
    var_glyph_cache: HashMap<VarGlyphCacheKey, VarGlyphBitmap>,
    font_system: FontSystem,
    scratch: ShapeBuffer,
    /// Contains all already loaded fonts, including all faces. Indexed by `FontId`.
    loaded_fonts: Vec<LoadedFont>,
    /// Caches the `FontId`s associated with a specific family to avoid iterating the font database
    /// for every font face in a family.
    font_ids_by_family_cache: HashMap<FontKey, SmallVec<[FontId; 4]>>,
    /// Caches variable-font weight instances `(base_font_id, rounded_wght)` -> derived `FontId`.
    variable_weight_fonts: HashMap<(FontId, u16), FontId>,
}

struct LoadedFont {
    font: Arc<CosmicTextFont>,
    features: CosmicFontFeatures,
    is_known_emoji_font: bool,
    variable: Option<VariableFontInstance>,
}

impl LoadedFont {
    fn norm_coords(&self) -> &[i16] {
        self.variable
            .as_ref()
            .map_or(&[], |var| var.norm_coords.as_slice())
    }
}

impl CosmicTextSystem {
    pub(crate) fn new() -> Self {
        // todo(linux) make font loading non-blocking
        let font_system = FontSystem::new();

        Self(RwLock::new(CosmicTextSystemState {
            instance_id: NEXT_TEXT_SYSTEM_ID.fetch_add(1, Ordering::Relaxed),
            font_system,
            swash_cache: SwashCache::new(),
            var_glyph_cache: HashMap::default(),
            scratch: ShapeBuffer::default(),
            loaded_fonts: Vec::new(),
            font_ids_by_family_cache: HashMap::default(),
            variable_weight_fonts: HashMap::default(),
        }))
    }
}

impl Default for CosmicTextSystem {
    fn default() -> Self {
        Self::new()
    }
}

impl PlatformTextSystem for CosmicTextSystem {
    fn add_fonts(&self, fonts: Vec<Cow<'static, [u8]>>) -> Result<()> {
        self.0.write().add_fonts(fonts)
    }

    fn all_font_names(&self) -> Vec<String> {
        let mut result = self
            .0
            .read()
            .font_system
            .db()
            .faces()
            .filter_map(|face| face.families.first().map(|family| family.0.clone()))
            .collect_vec();
        result.sort();
        result.dedup();
        result
    }

    fn font_id(&self, font: &Font) -> Result<FontId> {
        // todo(linux): Do we need to use CosmicText's Font APIs? Can we consolidate this to use font_kit?
        let mut state = self.0.write();
        let key = FontKey::new(font.family.clone(), font.features.clone());
        let candidates = if let Some(font_ids) = state.font_ids_by_family_cache.get(&key) {
            font_ids.as_slice()
        } else {
            let font_ids = state.load_family(&font.family, &font.features)?;
            state.font_ids_by_family_cache.insert(key.clone(), font_ids);
            state.font_ids_by_family_cache[&key].as_ref()
        };

        // todo(linux) ideally we would make fontdb's `find_best_match` pub instead of using font-kit here
        let candidate_properties = candidates
            .iter()
            .map(|font_id| {
                let database_id = state.loaded_font(*font_id).font.id();
                let face_info = state.font_system.db().face(database_id).expect("");
                face_info_into_properties(face_info)
            })
            .collect::<SmallVec<[_; 4]>>();

        let ix =
            font_kit::matching::find_best_match(&candidate_properties, &font_into_properties(font))
                .context("requested font family contains no font matching the other parameters")?;

        let base_font_id = candidates[ix];
        Ok(state.resolve_variable_weight_font(base_font_id, font.weight))
    }

    fn font_metrics(&self, font_id: FontId) -> FontMetrics {
        let lock = self.0.read();
        let loaded = lock.loaded_font(font_id);
        let metrics = loaded.font.as_swash().metrics(loaded.norm_coords());

        FontMetrics {
            units_per_em: metrics.units_per_em as u32,
            ascent: metrics.ascent,
            descent: -metrics.descent, // todo(linux) confirm this is correct
            line_gap: metrics.leading,
            underline_position: metrics.underline_offset,
            underline_thickness: metrics.stroke_size,
            cap_height: metrics.cap_height,
            x_height: metrics.x_height,
            // todo(linux): Compute this correctly
            bounding_box: Bounds {
                origin: point(0.0, 0.0),
                size: size(metrics.max_width, metrics.ascent + metrics.descent),
            },
        }
    }

    fn typographic_bounds(&self, font_id: FontId, glyph_id: GlyphId) -> Result<Bounds<f32>> {
        let lock = self.0.read();
        let loaded = lock.loaded_font(font_id);
        let glyph_metrics = loaded.font.as_swash().glyph_metrics(loaded.norm_coords());
        let glyph_id = glyph_id.0 as u16;
        // todo(linux): Compute this correctly
        // see https://github.com/servo/font-kit/blob/master/src/loaders/freetype.rs#L614-L620
        Ok(Bounds {
            origin: point(0.0, 0.0),
            size: size(
                glyph_metrics.advance_width(glyph_id),
                glyph_metrics.advance_height(glyph_id),
            ),
        })
    }

    fn advance(&self, font_id: FontId, glyph_id: GlyphId) -> Result<Size<f32>> {
        self.0.read().advance(font_id, glyph_id)
    }

    fn glyph_for_char(&self, font_id: FontId, ch: char) -> Option<GlyphId> {
        self.0.read().glyph_for_char(font_id, ch)
    }

    fn glyph_raster_bounds(&self, params: &RenderGlyphParams) -> Result<Bounds<DevicePixels>> {
        self.0.write().raster_bounds(params)
    }

    fn rasterize_glyph(
        &self,
        params: &RenderGlyphParams,
        raster_bounds: Bounds<DevicePixels>,
    ) -> Result<(Size<DevicePixels>, Vec<u8>)> {
        self.0.write().rasterize_glyph(params, raster_bounds)
    }

    fn layout_line(&self, text: &str, font_size: Pixels, runs: &[FontRun]) -> LineLayout {
        self.0.write().layout_line(text, font_size, runs)
    }
}

impl CosmicTextSystemState {
    fn loaded_font(&self, font_id: FontId) -> &LoadedFont {
        &self.loaded_fonts[font_id.0]
    }

    #[profiling::function]
    fn add_fonts(&mut self, fonts: Vec<Cow<'static, [u8]>>) -> Result<()> {
        let db = self.font_system.db_mut();
        for bytes in fonts {
            match bytes {
                Cow::Borrowed(embedded_font) => {
                    db.load_font_data(embedded_font.to_vec());
                }
                Cow::Owned(bytes) => {
                    db.load_font_data(bytes);
                }
            }
        }
        Ok(())
    }

    fn resolve_family_name(&self, name: &str) -> String {
        let db = self.font_system.db();
        let has_family = |candidate: &str| {
            db.faces()
                .any(|face| face.families.iter().any(|family| family.0 == candidate))
        };

        match name {
            ".SystemUIFont" | "system-ui" | "sans-serif" => {
                let configured = db.family_name(&Family::SansSerif);
                if has_family(configured) {
                    return configured.to_owned();
                }
                for fallback in [
                    "Ubuntu Sans",
                    "Ubuntu",
                    "Adwaita Sans",
                    "Cantarell",
                    "Noto Sans",
                    "IBM Plex Sans",
                    "DejaVu Sans",
                ] {
                    if has_family(fallback) {
                        return fallback.to_owned();
                    }
                }
                "IBM Plex Sans".to_owned()
            }
            "monospace" | "ui-monospace" => {
                let configured = db.family_name(&Family::Monospace);
                if has_family(configured) {
                    return configured.to_owned();
                }
                for fallback in [
                    "Ubuntu Sans Mono",
                    "Ubuntu Mono",
                    "DejaVu Sans Mono",
                    "Noto Sans Mono",
                    "Liberation Mono",
                ] {
                    if has_family(fallback) {
                        return fallback.to_owned();
                    }
                }
                "monospace".to_owned()
            }
            "serif" => {
                let configured = db.family_name(&Family::Serif);
                if has_family(configured) {
                    return configured.to_owned();
                }
                "serif".to_owned()
            }
            _ => crate::text_system::font_name_with_fallbacks(name, "IBM Plex Sans").to_owned(),
        }
    }

    fn resolve_variable_weight_font(&mut self, base_font_id: FontId, weight: FontWeight) -> FontId {
        let (cosmic_font, face_index, features, is_known_emoji_font) = {
            let base = self.loaded_font(base_font_id);
            let Some(face_info) = self.font_system.db().face(base.font.id()) else {
                return base_font_id;
            };
            (
                base.font.clone(),
                face_info.index,
                base.features.clone(),
                base.is_known_emoji_font,
            )
        };
        if is_known_emoji_font {
            return base_font_id;
        }
        let Some(axis) = inspect_variable_weight_axis(cosmic_font.data(), face_index) else {
            return base_font_id;
        };
        let clamped_weight = weight.0.clamp(axis.wght_min, axis.wght_max);
        let weight_key = clamped_weight.round() as u16;
        if let Some(&cached) = self.variable_weight_fonts.get(&(base_font_id, weight_key)) {
            return cached;
        }

        let mut design_coords = axis.default_coords;
        design_coords[axis.wght_index] = f32::from(weight_key);
        let norm_coords =
            compute_normalized_coords(cosmic_font.data(), face_index, f32::from(weight_key));

        let font_id = FontId(self.loaded_fonts.len());
        self.loaded_fonts.push(LoadedFont {
            font: cosmic_font,
            features,
            is_known_emoji_font,
            variable: Some(VariableFontInstance {
                face_index,
                design_coords,
                norm_coords,
            }),
        });
        self.variable_weight_fonts
            .insert((base_font_id, weight_key), font_id);
        font_id
    }

    #[profiling::function]
    fn load_family(
        &mut self,
        name: &str,
        features: &FontFeatures,
    ) -> Result<SmallVec<[FontId; 4]>> {
        let name = self.resolve_family_name(name);

        let families = self
            .font_system
            .db()
            .faces()
            .filter(|face| face.families.iter().any(|family| name == family.0))
            .map(|face| (face.id, face.post_script_name.clone()))
            .collect::<SmallVec<[_; 4]>>();

        let mut loaded_font_ids = SmallVec::new();
        for (font_id, postscript_name) in families {
            let font = self
                .font_system
                .get_font(font_id)
                .context("Could not load font")?;

            // HACK: To let the storybook run and render Windows caption icons. We should actually do better font fallback.
            let allowed_bad_font_names = [
                "SegoeFluentIcons", // NOTE: Segoe fluent icons postscript name is inconsistent
                "Segoe Fluent Icons",
            ];

            if font.as_swash().charmap().map('m') == 0
                && !allowed_bad_font_names.contains(&postscript_name.as_str())
            {
                self.font_system.db_mut().remove_face(font.id());
                continue;
            };

            let font_id = FontId(self.loaded_fonts.len());
            loaded_font_ids.push(font_id);
            self.loaded_fonts.push(LoadedFont {
                font,
                features: features.try_into()?,
                is_known_emoji_font: check_is_known_emoji_font(&postscript_name),
                variable: None,
            });
        }

        Ok(loaded_font_ids)
    }

    fn advance(&self, font_id: FontId, glyph_id: GlyphId) -> Result<Size<f32>> {
        let loaded = self.loaded_font(font_id);
        let glyph_metrics = loaded.font.as_swash().glyph_metrics(loaded.norm_coords());
        Ok(Size {
            width: glyph_metrics.advance_width(glyph_id.0 as u16),
            height: glyph_metrics.advance_height(glyph_id.0 as u16),
        })
    }

    fn glyph_for_char(&self, font_id: FontId, ch: char) -> Option<GlyphId> {
        let glyph_id = self.loaded_font(font_id).font.as_swash().charmap().map(ch);
        if glyph_id == 0 {
            None
        } else {
            Some(GlyphId(glyph_id.into()))
        }
    }

    fn get_or_rasterize_variable_glyph(
        &mut self,
        params: &RenderGlyphParams,
    ) -> Result<&VarGlyphBitmap> {
        let px_size: f32 = (params.font_size * params.scale_factor).into();
        let key = VarGlyphCacheKey {
            font_id: params.font_id,
            glyph_id: params.glyph_id.0,
            font_size_bits: px_size.to_bits(),
            subpixel_x: params.subpixel_variant.x,
            subpixel_y: params.subpixel_variant.y,
        };
        if !self.var_glyph_cache.contains_key(&key) {
            let loaded = &self.loaded_fonts[params.font_id.0];
            let var = loaded
                .variable
                .as_ref()
                .context("variable glyph requested for non-variable font")?;
            let subpixel_shift = point(
                params.subpixel_variant.x as f32 / SUBPIXEL_VARIANTS_X as f32 / params.scale_factor,
                params.subpixel_variant.y as f32 / SUBPIXEL_VARIANTS_Y as f32 / params.scale_factor,
            );
            let bitmap = rasterize_variable_glyph_freetype(
                self.instance_id,
                params.font_id,
                loaded.font.data(),
                var.face_index,
                &var.design_coords,
                params.glyph_id.0,
                px_size,
                subpixel_shift.x,
                subpixel_shift.y.trunc(),
            )?;
            self.var_glyph_cache.insert(key, bitmap);
        }
        Ok(&self.var_glyph_cache[&key])
    }

    fn raster_bounds(&mut self, params: &RenderGlyphParams) -> Result<Bounds<DevicePixels>> {
        if !params.is_emoji && self.loaded_fonts[params.font_id.0].variable.is_some() {
            let bmp = self.get_or_rasterize_variable_glyph(params)?;
            return Ok(Bounds {
                origin: point(bmp.origin_x.into(), bmp.origin_y.into()),
                size: size(bmp.width.into(), bmp.height.into()),
            });
        }
        let font = &self.loaded_fonts[params.font_id.0].font;
        let subpixel_shift = point(
            params.subpixel_variant.x as f32 / SUBPIXEL_VARIANTS_X as f32 / params.scale_factor,
            params.subpixel_variant.y as f32 / SUBPIXEL_VARIANTS_Y as f32 / params.scale_factor,
        );
        let image = self
            .swash_cache
            .get_image(
                &mut self.font_system,
                CacheKey::new(
                    font.id(),
                    params.glyph_id.0 as u16,
                    (params.font_size * params.scale_factor).into(),
                    (subpixel_shift.x, subpixel_shift.y.trunc()),
                    cosmic_text::CacheKeyFlags::empty(),
                )
                .0,
            )
            .clone()
            .with_context(|| format!("no image for {params:?} in font {font:?}"))?;
        Ok(Bounds {
            origin: point(image.placement.left.into(), (-image.placement.top).into()),
            size: size(image.placement.width.into(), image.placement.height.into()),
        })
    }

    #[profiling::function]
    fn rasterize_glyph(
        &mut self,
        params: &RenderGlyphParams,
        glyph_bounds: Bounds<DevicePixels>,
    ) -> Result<(Size<DevicePixels>, Vec<u8>)> {
        if glyph_bounds.size.width.0 == 0 || glyph_bounds.size.height.0 == 0 {
            anyhow::bail!("glyph bounds are empty");
        } else if !params.is_emoji && self.loaded_fonts[params.font_id.0].variable.is_some() {
            let bmp = self.get_or_rasterize_variable_glyph(params)?;
            if bmp.width <= 0 || bmp.height <= 0 {
                anyhow::bail!("glyph bounds are empty");
            }
            Ok((size(bmp.width.into(), bmp.height.into()), bmp.data.clone()))
        } else {
            let bitmap_size = glyph_bounds.size;
            let font = &self.loaded_fonts[params.font_id.0].font;
            let subpixel_shift = point(
                params.subpixel_variant.x as f32 / SUBPIXEL_VARIANTS_X as f32 / params.scale_factor,
                params.subpixel_variant.y as f32 / SUBPIXEL_VARIANTS_Y as f32 / params.scale_factor,
            );
            let mut image = self
                .swash_cache
                .get_image(
                    &mut self.font_system,
                    CacheKey::new(
                        font.id(),
                        params.glyph_id.0 as u16,
                        (params.font_size * params.scale_factor).into(),
                        (subpixel_shift.x, subpixel_shift.y.trunc()),
                        cosmic_text::CacheKeyFlags::empty(),
                    )
                    .0,
                )
                .clone()
                .with_context(|| format!("no image for {params:?} in font {font:?}"))?;

            if params.is_emoji {
                // Convert from RGBA to BGRA.
                for pixel in image.data.chunks_exact_mut(4) {
                    pixel.swap(0, 2);
                }
            }

            Ok((bitmap_size, image.data))
        }
    }

    /// This is used when cosmic_text has chosen a fallback font instead of using the requested
    /// font, typically to handle some unicode characters. When this happens, `loaded_fonts` may not
    /// yet have an entry for this fallback font, and so one is added.
    ///
    /// Note that callers shouldn't use this `FontId` somewhere that will retrieve the corresponding
    /// `LoadedFont.features`, as it will have an arbitrarily chosen or empty value. The only
    /// current use of this field is for the *input* of `layout_line`, and so it's fine to use
    /// `font_id_for_cosmic_id` when computing the *output* of `layout_line`.
    fn font_id_for_cosmic_id(&mut self, id: cosmic_text::fontdb::ID) -> FontId {
        if let Some(ix) = self
            .loaded_fonts
            .iter()
            .position(|loaded_font| loaded_font.font.id() == id)
        {
            FontId(ix)
        } else {
            let font = self.font_system.get_font(id).unwrap();
            let face = self.font_system.db().face(id).unwrap();

            let font_id = FontId(self.loaded_fonts.len());
            self.loaded_fonts.push(LoadedFont {
                font,
                features: CosmicFontFeatures::new(),
                is_known_emoji_font: check_is_known_emoji_font(&face.post_script_name),
                variable: None,
            });

            font_id
        }
    }

    #[profiling::function]
    fn layout_line(&mut self, text: &str, font_size: Pixels, font_runs: &[FontRun]) -> LineLayout {
        let mut attrs_list = AttrsList::new(&Attrs::new());
        let mut offs = 0;
        for run in font_runs {
            let loaded_font = self.loaded_font(run.font_id);
            let font = self.font_system.db().face(loaded_font.font.id()).unwrap();

            attrs_list.add_span(
                offs..(offs + run.len),
                &Attrs::new()
                    .metadata(run.font_id.0)
                    .family(Family::Name(&font.families.first().unwrap().0))
                    .stretch(font.stretch)
                    .style(font.style)
                    .weight(font.weight)
                    .font_features(loaded_font.features.clone()),
            );
            offs += run.len;
        }

        let line = ShapeLine::new(
            &mut self.font_system,
            text,
            &attrs_list,
            cosmic_text::Shaping::Advanced,
            4,
        );
        let mut layout_lines = Vec::with_capacity(1);
        line.layout_to_buffer(
            &mut self.scratch,
            font_size.0,
            None, // We do our own wrapping
            cosmic_text::Wrap::None,
            None,
            &mut layout_lines,
            None,
        );
        let layout = layout_lines.first().unwrap();

        let mut runs: Vec<ShapedRun> = Vec::new();
        let mut variable_advance_delta = 0.0f32;
        for glyph in &layout.glyphs {
            let mut font_id = FontId(glyph.metadata);
            let mut loaded_font = self.loaded_font(font_id);
            if loaded_font.font.id() != glyph.font_id {
                font_id = self.font_id_for_cosmic_id(glyph.font_id);
                loaded_font = self.loaded_font(font_id);
            }
            let is_emoji = loaded_font.is_known_emoji_font;

            // HACK: Prevent crash caused by variation selectors.
            if glyph.glyph_id == 3 && is_emoji {
                continue;
            }

            let shaped_glyph = ShapedGlyph {
                id: GlyphId(glyph.glyph_id as u32),
                position: point((glyph.x + variable_advance_delta).into(), glyph.y.into()),
                index: glyph.start,
                is_emoji,
            };

            let norm_coords = loaded_font.norm_coords();
            if !norm_coords.is_empty() {
                let swash_font = loaded_font.font.as_swash();
                let upem = f32::from(swash_font.metrics(&[]).units_per_em);
                if upem > 0.0 {
                    let default_adv = swash_font.glyph_metrics(&[]).advance_width(glyph.glyph_id);
                    let varied_adv = swash_font
                        .glyph_metrics(norm_coords)
                        .advance_width(glyph.glyph_id);
                    variable_advance_delta += (varied_adv - default_adv) * (font_size.0 / upem);
                }
            }

            if let Some(last_run) = runs
                .last_mut()
                .filter(|last_run| last_run.font_id == font_id)
            {
                last_run.glyphs.push(shaped_glyph);
            } else {
                runs.push(ShapedRun {
                    font_id,
                    glyphs: vec![shaped_glyph],
                });
            }
        }

        LineLayout {
            font_size,
            width: (layout.w + variable_advance_delta).max(0.0).into(),
            ascent: layout.max_ascent.into(),
            descent: layout.max_descent.into(),
            runs,
            len: text.len(),
        }
    }
}

impl TryFrom<&FontFeatures> for CosmicFontFeatures {
    type Error = anyhow::Error;

    fn try_from(features: &FontFeatures) -> Result<Self> {
        let mut result = CosmicFontFeatures::new();
        for feature in features.0.iter() {
            let name_bytes: [u8; 4] = feature
                .0
                .as_bytes()
                .try_into()
                .context("Incorrect feature flag format")?;

            let tag = cosmic_text::FeatureTag::new(&name_bytes);

            result.set(tag, feature.1);
        }
        Ok(result)
    }
}

impl From<RectF> for Bounds<f32> {
    fn from(rect: RectF) -> Self {
        Bounds {
            origin: point(rect.origin_x(), rect.origin_y()),
            size: size(rect.width(), rect.height()),
        }
    }
}

impl From<RectI> for Bounds<DevicePixels> {
    fn from(rect: RectI) -> Self {
        Bounds {
            origin: point(DevicePixels(rect.origin_x()), DevicePixels(rect.origin_y())),
            size: size(DevicePixels(rect.width()), DevicePixels(rect.height())),
        }
    }
}

impl From<Vector2I> for Size<DevicePixels> {
    fn from(value: Vector2I) -> Self {
        size(value.x().into(), value.y().into())
    }
}

impl From<RectI> for Bounds<i32> {
    fn from(rect: RectI) -> Self {
        Bounds {
            origin: point(rect.origin_x(), rect.origin_y()),
            size: size(rect.width(), rect.height()),
        }
    }
}

impl From<Point<u32>> for Vector2I {
    fn from(size: Point<u32>) -> Self {
        Vector2I::new(size.x as i32, size.y as i32)
    }
}

impl From<Vector2F> for Size<f32> {
    fn from(vec: Vector2F) -> Self {
        size(vec.x(), vec.y())
    }
}

impl From<FontWeight> for cosmic_text::Weight {
    fn from(value: FontWeight) -> Self {
        cosmic_text::Weight(value.0 as u16)
    }
}

impl From<FontStyle> for cosmic_text::Style {
    fn from(style: FontStyle) -> Self {
        match style {
            FontStyle::Normal => cosmic_text::Style::Normal,
            FontStyle::Italic => cosmic_text::Style::Italic,
            FontStyle::Oblique => cosmic_text::Style::Oblique,
        }
    }
}

fn font_into_properties(font: &crate::Font) -> font_kit::properties::Properties {
    font_kit::properties::Properties {
        style: match font.style {
            crate::FontStyle::Normal => font_kit::properties::Style::Normal,
            crate::FontStyle::Italic => font_kit::properties::Style::Italic,
            crate::FontStyle::Oblique => font_kit::properties::Style::Oblique,
        },
        weight: font_kit::properties::Weight(font.weight.0),
        stretch: Default::default(),
    }
}

fn face_info_into_properties(
    face_info: &cosmic_text::fontdb::FaceInfo,
) -> font_kit::properties::Properties {
    font_kit::properties::Properties {
        style: match face_info.style {
            cosmic_text::Style::Normal => font_kit::properties::Style::Normal,
            cosmic_text::Style::Italic => font_kit::properties::Style::Italic,
            cosmic_text::Style::Oblique => font_kit::properties::Style::Oblique,
        },
        // both libs use the same values for weight
        weight: font_kit::properties::Weight(face_info.weight.0.into()),
        stretch: match face_info.stretch {
            cosmic_text::Stretch::Condensed => font_kit::properties::Stretch::CONDENSED,
            cosmic_text::Stretch::Expanded => font_kit::properties::Stretch::EXPANDED,
            cosmic_text::Stretch::ExtraCondensed => font_kit::properties::Stretch::EXTRA_CONDENSED,
            cosmic_text::Stretch::ExtraExpanded => font_kit::properties::Stretch::EXTRA_EXPANDED,
            cosmic_text::Stretch::Normal => font_kit::properties::Stretch::NORMAL,
            cosmic_text::Stretch::SemiCondensed => font_kit::properties::Stretch::SEMI_CONDENSED,
            cosmic_text::Stretch::SemiExpanded => font_kit::properties::Stretch::SEMI_EXPANDED,
            cosmic_text::Stretch::UltraCondensed => font_kit::properties::Stretch::ULTRA_CONDENSED,
            cosmic_text::Stretch::UltraExpanded => font_kit::properties::Stretch::ULTRA_EXPANDED,
        },
    }
}

fn inspect_variable_weight_axis(data: &[u8], face_index: u32) -> Option<VariableWeightAxis> {
    let face = cosmic_text::ttf_parser::Face::parse(data, face_index).ok()?;
    if !face.is_variable() {
        return None;
    }
    let mut default_coords = SmallVec::new();
    let mut wght_info = None;
    let wght_tag = cosmic_text::ttf_parser::Tag::from_bytes(b"wght");
    for (idx, axis) in face.variation_axes().into_iter().enumerate() {
        default_coords.push(axis.def_value);
        if axis.tag == wght_tag {
            wght_info = Some((idx, axis.min_value, axis.max_value));
        }
    }
    let (wght_index, wght_min, wght_max) = wght_info?;
    if (wght_max - wght_min).abs() < f32::EPSILON {
        return None;
    }
    Some(VariableWeightAxis {
        default_coords,
        wght_index,
        wght_min,
        wght_max,
    })
}

fn compute_normalized_coords(data: &[u8], face_index: u32, weight: f32) -> SmallVec<[i16; 4]> {
    let std::result::Result::Ok(mut face) = cosmic_text::ttf_parser::Face::parse(data, face_index)
    else {
        return SmallVec::new();
    };
    let _ = face.set_variation(cosmic_text::ttf_parser::Tag::from_bytes(b"wght"), weight);
    face.variation_coordinates()
        .iter()
        .map(|coord| coord.get())
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn rasterize_variable_glyph_freetype(
    instance_id: u64,
    font_id: FontId,
    font_data: &[u8],
    face_index: u32,
    design_coords: &[f32],
    glyph_id: u32,
    px_size: f32,
    subpixel_x: f32,
    subpixel_y: f32,
) -> Result<VarGlyphBitmap> {
    VARIABLE_FREETYPE_FONTS.with(|cell| {
        let mut cache = cell.borrow_mut();
        let ft_font = match cache.entry((instance_id, font_id)) {
            std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::hash_map::Entry::Vacant(entry) => {
                let ft_font = FreetypeFont::from_bytes(Arc::new(font_data.to_vec()), face_index)
                    .map_err(|err| {
                        anyhow::anyhow!("failed to open FreeType variable font: {err:?}")
                    })?;
                let fixed_coords: SmallVec<[libc::c_long; 4]> = design_coords
                    .iter()
                    .map(|&v| (v * 65536.0).round() as libc::c_long)
                    .collect();
                let native = ft_font.native_font();
                let err = unsafe {
                    let res = FT_Set_Var_Design_Coordinates(
                        native,
                        fixed_coords.len() as libc::c_uint,
                        fixed_coords.as_ptr(),
                    );
                    FT_Done_Face(native);
                    res
                };
                if err != 0 {
                    anyhow::bail!("FT_Set_Var_Design_Coordinates failed with error {err}");
                }
                entry.insert(ft_font)
            }
        };

        let subpixel_transform =
            Transform2F::from_translation(Vector2F::new(subpixel_x, subpixel_y));
        let est_bounds = ft_font
            .raster_bounds(
                glyph_id,
                px_size,
                subpixel_transform,
                HintingOptions::Vertical(px_size),
                RasterizationOptions::GrayscaleAa,
            )
            .map_err(|err| anyhow::anyhow!("FreeType raster_bounds failed: {err:?}"))?;

        if est_bounds.width() <= 0 || est_bounds.height() <= 0 {
            return Ok(VarGlyphBitmap {
                origin_x: 0,
                origin_y: 0,
                width: 0,
                height: 0,
                data: Vec::new(),
            });
        }

        let pad = 2;
        let padded_origin = Vector2I::new(est_bounds.origin_x() - pad, est_bounds.origin_y() - pad);
        let padded_size =
            Vector2I::new(est_bounds.width() + pad * 2, est_bounds.height() + pad * 2);
        let mut canvas = Canvas::new(padded_size, CanvasFormat::A8);
        let draw_transform = Transform2F::from_translation(Vector2F::new(
            subpixel_x - padded_origin.x() as f32,
            subpixel_y - padded_origin.y() as f32,
        ));
        ft_font
            .rasterize_glyph(
                &mut canvas,
                glyph_id,
                px_size,
                draw_transform,
                HintingOptions::Vertical(px_size),
                RasterizationOptions::GrayscaleAa,
            )
            .map_err(|err| anyhow::anyhow!("FreeType rasterize_glyph failed: {err:?}"))?;

        let canvas_w = canvas.size.x() as usize;
        let canvas_h = canvas.size.y() as usize;
        let mut min_x = canvas_w;
        let mut min_y = canvas_h;
        let mut max_x = 0usize;
        let mut max_y = 0usize;
        for y in 0..canvas_h {
            let row = &canvas.pixels[y * canvas.stride..y * canvas.stride + canvas_w];
            for (x, &alpha) in row.iter().enumerate() {
                if alpha != 0 {
                    min_x = min_x.min(x);
                    min_y = min_y.min(y);
                    max_x = max_x.max(x);
                    max_y = max_y.max(y);
                }
            }
        }

        if min_x > max_x || min_y > max_y {
            return Ok(VarGlyphBitmap {
                origin_x: 0,
                origin_y: 0,
                width: 0,
                height: 0,
                data: Vec::new(),
            });
        }

        let tight_w = max_x - min_x + 1;
        let tight_h = max_y - min_y + 1;
        let mut data = Vec::with_capacity(tight_w * tight_h);
        for y in min_y..=max_y {
            let row = &canvas.pixels[y * canvas.stride + min_x..y * canvas.stride + max_x + 1];
            data.extend_from_slice(row);
        }

        Ok(VarGlyphBitmap {
            origin_x: padded_origin.x() + min_x as i32,
            origin_y: padded_origin.y() + min_y as i32,
            width: tight_w as i32,
            height: tight_h as i32,
            data,
        })
    })
}

fn check_is_known_emoji_font(postscript_name: &str) -> bool {
    // TODO: Include other common emoji fonts
    postscript_name == "NotoColorEmoji"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_system_ui_and_monospace_fonts_with_variable_weight_rasterization() {
        let text_system = CosmicTextSystem::new();
        let make_font = |family: &str, weight: FontWeight| Font {
            family: family.to_owned().into(),
            features: FontFeatures::default(),
            fallbacks: None,
            weight,
            style: FontStyle::Normal,
        };

        let mono_id = text_system
            .font_id(&make_font("monospace", FontWeight::NORMAL))
            .expect("monospace should resolve on Linux");
        assert!(text_system.glyph_for_char(mono_id, '0').is_some());

        let thin_id = text_system
            .font_id(&make_font(".SystemUIFont", FontWeight::THIN))
            .expect(".SystemUIFont thin should resolve");
        let regular_id = text_system
            .font_id(&make_font(".SystemUIFont", FontWeight::NORMAL))
            .expect(".SystemUIFont normal should resolve");
        let semibold_id = text_system
            .font_id(&make_font(".SystemUIFont", FontWeight::SEMIBOLD))
            .expect(".SystemUIFont semibold should resolve");
        let bold_id = text_system
            .font_id(&make_font(".SystemUIFont", FontWeight::BOLD))
            .expect(".SystemUIFont bold should resolve");

        assert_ne!(regular_id, bold_id);

        let raster_ink = |font_id: FontId| -> u64 {
            let glyph_id = text_system.glyph_for_char(font_id, 'H').unwrap();
            let params = RenderGlyphParams {
                font_id,
                glyph_id,
                font_size: crate::px(24.0),
                subpixel_variant: Point { x: 0, y: 0 },
                scale_factor: 1.0,
                is_emoji: false,
            };
            let bounds = text_system.glyph_raster_bounds(&params).unwrap();
            let (sz, bytes) = text_system.rasterize_glyph(&params, bounds).unwrap();
            assert_eq!(sz, bounds.size);
            assert_eq!(bytes.len(), (sz.width.0 * sz.height.0) as usize);
            bytes.into_iter().map(u64::from).sum()
        };

        let regular_ink = raster_ink(regular_id);
        let bold_ink = raster_ink(bold_id);
        assert!(
            bold_ink > regular_ink,
            "expected bold ink ({bold_ink}) > regular ink ({regular_ink})"
        );

        // When backed by a variable font (e.g. Ubuntu Sans on Ubuntu 26.04), intermediate weights
        // must also render with distinct stroke weights.
        if text_system
            .0
            .read()
            .loaded_font(regular_id)
            .variable
            .is_some()
        {
            let thin_ink = raster_ink(thin_id);
            let semibold_ink = raster_ink(semibold_id);
            assert_ne!(thin_id, regular_id);
            assert_ne!(regular_id, semibold_id);
            assert_ne!(semibold_id, bold_id);
            assert!(
                thin_ink < regular_ink && regular_ink < semibold_ink && semibold_ink < bold_ink,
                "expected monotonic variable weights: thin={thin_ink} < regular={regular_ink} < semibold={semibold_ink} < bold={bold_ink}"
            );
        }
    }
}
