extern crate sdl2;
extern crate rusttype;
extern crate unicode_normalization;

use sdl2::event::Event;
use sdl2::event::WindowEvent;
use sdl2::keyboard::Keycode;
use sdl2::pixels::Color;
use sdl2::rect::Rect;
use std::cmp;
use std::time::Duration;
use sdl2::pixels::PixelFormatEnum::{RGBA4444};
use sdl2::render::{TextureAccess::Streaming, Texture, BlendMode};
use sdl2::video::{Window};
use rusttype::{point, Font, FontCollection, PositionedGlyph, Scale};
use rusttype::gpu_cache::{CacheBuilder, Cache};

type Canvas = sdl2::render::Canvas<sdl2::video::Window>;

static BOX_W : i32 = 400;
static BOX_H : i32 = 300;

static TEXT: &str = "A japanese poem:\r
\r
色は匂へど散りぬるを我が世誰ぞ常ならむ有為の奥山今日越えて浅き夢見じ酔ひもせず\r
\r
Feel free to type out some text, and delete it with Backspace. \
You can also try resizing this window.";

fn layout_paragraph<'a>(
    font: &'a Font,
    scale: Scale,
    width: u32,
    text: &str,
) -> Vec<PositionedGlyph<'a>> {
    use unicode_normalization::UnicodeNormalization;
    let mut result = Vec::new();
    let v_metrics = font.v_metrics(scale);
    let advance_height = v_metrics.ascent - v_metrics.descent + v_metrics.line_gap;
    let mut caret = point(0.0, v_metrics.ascent);
    let mut last_glyph_id = None;
    for c in text.nfc() {
        if c.is_control() {
            match c {
                '\r' => {
                    caret = point(0.0, caret.y + advance_height);
                }
                '\n' => {}
                _ => {}
            }
            continue;
        }
        let base_glyph = font.glyph(c);
        if let Some(id) = last_glyph_id.take() {
            caret.x += font.pair_kerning(scale, id, base_glyph.id());
        }
        last_glyph_id = Some(base_glyph.id());
        let mut glyph = base_glyph.scaled(scale).positioned(caret);
        if let Some(bb) = glyph.pixel_bounding_box() {
            if bb.max.x > width as i32 {
                caret = point(0.0, caret.y + advance_height);
                glyph = glyph.into_unpositioned().positioned(caret);
                last_glyph_id = None;
            }
        }
        caret.x += glyph.unpositioned().h_metrics().advance_width;
        result.push(glyph);
    }
    result
}

fn draw_text<'l>(
  canvas : &mut Canvas, font : &'l Font, text : &str, x : i32, y : i32, scale : Scale,
  cache : &mut Cache<'l>, cache_width : u32, cache_height : u32, cache_tex : &mut Texture)
{
  let glyphs = layout_paragraph(&font, scale, BOX_W as u32, text);
  for glyph in &glyphs {
      cache.queue_glyph(0, glyph.clone());
  }
  cache
    .cache_queued(|rect, data| {
        let r =
          Rect::new(
            rect.min.x as i32,
            rect.min.y as i32,
            rect.width() as u32,
            rect.height() as u32);
        
        cache_tex.with_lock(Some(r), |target, pitch|{
          let (w, h) = (r.width() as usize, r.height() as usize);
          for y in 0..(h-1) {
            let off = y * pitch;
            for x in 0..(w-1) {
              let off = off + (x * 2);
              let v = data[w * y + x] >> 4;
              target[off] = 0x00 | v; // Blue, Alpha
              target[off + 1] = 0xF0; // Red, Green
            }
          }
        }).unwrap();
    })
    .unwrap();

  let (cw, ch) = (cache_width as f32, cache_height as f32);
  for g in glyphs.iter() {
    if let Ok(Some((uv_rect, offset_rect))) = cache.rect_for(0, g) {
        let screen_rect = Rect::new(
          x + offset_rect.min.x,
          y + offset_rect.min.y,
          offset_rect.width() as u32,
          offset_rect.height() as u32);
        let source_rect = Rect::new(
          (uv_rect.min.x * cw) as i32,
          (uv_rect.min.y * ch) as i32,
          (uv_rect.width() * cw) as u32,
          (uv_rect.height() * ch) as u32);
        canvas.copy(&cache_tex, Some(source_rect), Some(screen_rect)).unwrap();
    }
  }
}

fn dpi_ratio(w : &Window) -> f32 {
  let (dw, _) = w.drawable_size();
  let (w, _) = w.size();
  (w as f32) / (dw as f32)
}

pub fn main() {

	let (mut width, mut height) = (800, 600);

  let sdl_context = sdl2::init().unwrap();
  let video_subsystem = sdl_context.video().unwrap();

  let window = video_subsystem.window("cauldron", width, height)
    .position_centered()
    .resizable()
    .build()
    .unwrap();

  let dpi_ratio = dpi_ratio(&window);

  let mut canvas = window.into_canvas().accelerated().build().unwrap();

  canvas.set_blend_mode(BlendMode::Blend);

  canvas.clear();
  canvas.present();

  let mut events = sdl_context.event_pump().unwrap();

  let mut xd = 0;
  let mut yd = 0;

  let mut rects = vec!();

  let text : String = TEXT.into();

  // #### Font stuff ####
  //let font_data = include_bytes!("../fonts/consola.ttf");

  let font_data = include_bytes!("../fonts/msgothic.ttc");
  let collection = FontCollection::from_bytes(font_data as &[u8]).unwrap_or_else(|e| {
    panic!("error constructing a FontCollection from bytes: {}", e);
  });
  let font = collection.font_at(0) // only succeeds if collection consists of one font
    .unwrap_or_else(|e| {
      panic!("error turning FontCollection into a Font: {}", e);
    });

  let (cache_width, cache_height) = (512 * dpi_ratio as u32, 512 * dpi_ratio as u32);
  let mut cache = CacheBuilder {
      width: cache_width,
      height: cache_height,
      ..CacheBuilder::default()
  }.build();

  let texture_creator = canvas.texture_creator();
  let mut cache_tex = texture_creator.create_texture(RGBA4444, Streaming, cache_width, cache_height).unwrap();
  cache_tex.set_blend_mode(BlendMode::Blend);

  'mainloop: loop {
    for event in events.poll_iter() {
      match event {
        Event::Quit{..} |
        Event::KeyDown {keycode: Some(Keycode::Escape), ..} =>
            break 'mainloop,
        Event::MouseButtonUp {x, y, ..} => {
          let xp = cmp::min(x, xd);
          let yp = cmp::min(y, yd);
          let w = (x - xd).abs() as u32;
          let h = (y - yd).abs() as u32;
          rects.push(Rect::new(xp, yp, w, h));
        },
        Event::MouseButtonDown {x, y, ..} => {
            xd = x;
            yd = y;
        },
        Event::Window { win_event, .. } => {
          match win_event {
            WindowEvent::Resized(x, y) => {
              width = x as u32;
              height = y as u32;
            },
            _ => {}
          }
        },
        _e => {}
      }
    }
    ::std::thread::sleep(Duration::new(0, 1_000_000_000u32 / 60));
    // The rest of the loop goes here...

    canvas.set_draw_color(Color::RGBA(0, 0, 0, 255));
    canvas.clear();
    canvas.set_draw_color(Color::RGBA(255, 255, 255, 255));
    for r in rects.iter() {
      canvas.fill_rect(*r).unwrap();
    }

    let (rw, rh) = (BOX_W, BOX_H);
    let tx = (width/2) as i32 - (rw/2);
    let ty = (height/2) as i32 - (rh/2);
    canvas.fill_rect(Rect::new(tx, ty, rw as u32, rh as u32)).unwrap();

    let scale = Scale::uniform(24.0 * dpi_ratio);
    draw_text(
      &mut canvas, &font, &text, tx, ty, scale,
      &mut cache, cache_width, cache_height, &mut cache_tex);

    canvas.present();
	}
}

