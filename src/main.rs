extern crate sdl2;
extern crate rusttype;
extern crate unicode_normalization;
extern crate ropey;

use sdl2::event::Event;
use sdl2::event::WindowEvent;
use sdl2::keyboard::{Keycode, Scancode};
use sdl2::pixels::Color;
use sdl2::rect::Rect;
use std::cmp;
use std::time::Duration;
use sdl2::pixels::PixelFormatEnum::{RGBA4444};
use sdl2::render::{TextureAccess::Streaming, Texture, BlendMode};
use sdl2::video::{Window};
use rusttype::{point, Font, FontCollection, PositionedGlyph, Scale, VMetrics};
use rusttype::gpu_cache::{CacheBuilder, Cache};
use ropey::Rope;

type Canvas = sdl2::render::Canvas<sdl2::video::Window>;

static BOX_W : i32 = 400;
static BOX_H : i32 = 300;

static TEXT: &str = "Here is some text.\r

cit\u{0065}\u{0301}  <<< this tests grapheme correctness

Feel free to type stuff.\r
And delete it with Backspace.";

struct LayoutAttribs {
  advance_width : f32,
  advance_height : f32,
  v_metrics : VMetrics,
  buffer_width : f32,
  scale : Scale,
}

fn layout_attribs(font : &Font, scale : Scale, buffer_width : f32) -> LayoutAttribs {
  let v_metrics = font.v_metrics(scale);
  LayoutAttribs {
    advance_height: v_metrics.ascent - v_metrics.descent + v_metrics.line_gap,
    advance_width: {
      let g = font.glyph('a').scaled(scale);
      let g_width = g.h_metrics().advance_width;
      let kern = font.pair_kerning(scale, g.id(), g.id());
      g_width + kern
    },
    v_metrics,
    buffer_width,
    scale
  }
}

fn layout_paragraph<'a>(
  font: &'a Font,
  attribs : &LayoutAttribs,
  text_buffer : &Rope)
    -> Vec<PositionedGlyph<'a>>
{
    use unicode_normalization::UnicodeNormalization;
    let mut result = Vec::new();
    let mut caret = point(0.0, attribs.v_metrics.ascent);

    for l in text_buffer.lines() {
      // TODO: I'm not convinced that this handles multi-codepoint glyphs properly. Maybe the nfc function does.
      for c in l.chars().nfc() {
        if c.is_control() {
          continue;
        }
        let base_glyph = font.glyph(c);
        let mut glyph = base_glyph.scaled(attribs.scale).positioned(caret);
        caret.x += attribs.advance_width;
        result.push(glyph);
      }
      caret = point(0.0, caret.y + attribs.advance_height);
    }
    result
}

// TODO: this takes way too many paramters, so there should probably be some structs or something
fn draw_text<'l>(
  canvas : &mut Canvas, font : &'l Font, text_buffer : &Rope, attribs : &LayoutAttribs,
  cache : &mut Cache<'l>, cache_width : u32, cache_height : u32, cache_tex : &mut Texture)
{
  let glyphs = layout_paragraph(&font, attribs, text_buffer);
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
        
        // TODO: this may be very inefficient. Not sure.
        cache_tex.with_lock(Some(r), |target, pitch|{
          let (w, h) = (r.width() as usize, r.height() as usize);
          for y in 0..h {
            let off = y * pitch;
            for x in 0..w {
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
          offset_rect.min.x,
          offset_rect.min.y,
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

/*

- Receive some keyboard events
- Move a caret around
  - How is the caret's position represented?
    - Absolute index in text?
    - Line number and offset?

*/

// TODO: Pos might sometimes be changed in terms of graphemes instead
// codepoints. Not sure.
struct Caret {
  pos : usize,
  pos_remembered : usize,
  marker : Option<usize>,
}

fn step_right(c : &mut Caret, text : &Rope){
  if c.pos < text.len_chars() {
    c.pos = text.next_grapheme_boundary(c.pos);
  }
  c.pos_remembered = c.pos;
}

fn step_left(c : &mut Caret, text : &Rope){
  if c.pos > 0 {
    c.pos = text.prev_grapheme_boundary(c.pos);
  }
  c.pos_remembered = c.pos;
}

/*
  This is basically a replacement for the "char_to_line" function
  which behaves in a more convenient way for me. The default function
  will imagine a newline at the end of the rope, even when there is no
  newline character. This function very crudely corrects that, but
  should also be treated as suspicious and possibly broken.
*/
fn char_to_line(text : &Rope, pos : usize) -> usize {
  let l = text.char_to_line(pos);
  if l == text.len_lines() { l - 1 }
  else { l }
}

fn line_change(c : &mut Caret, text : &Rope, dir : i32){
  let new_line = char_to_line(text, c.pos) as i32 + dir;
  if new_line < 0 || new_line >= (text.len_lines() as i32) {
    return;
  }

  let mut line_offset_graphemes = {
    let line_start_pos = text.line_to_char(char_to_line(text, c.pos_remembered));
    text.slice(line_start_pos..c.pos_remembered).graphemes().count()
  };

  let line = text.line(new_line as usize);
  let new_line_graphemes = line.graphemes().count() - 1;
  if line_offset_graphemes > new_line_graphemes {
    line_offset_graphemes = cmp::max(0, new_line_graphemes);
  }

  let mut pos = 0;
  while line_offset_graphemes > 0 && pos < line.len_chars() {
    pos = line.next_grapheme_boundary(pos);
    line_offset_graphemes -= 1;
  }
  c.pos = text.line_to_char(new_line as usize) + pos;
}

fn step_up(c : &mut Caret, text : &Rope){
  line_change(c, text, -1);
}

fn step_down(c : &mut Caret, text : &Rope){
  line_change(c, text, 1);
}

fn insert_text(caret : &mut Caret, text_buffer : &mut Rope, text : &str){
  text_buffer.insert(caret.pos, text);
  caret.pos += text.len();
}

fn backspace_text(caret : &mut Caret, text_buffer : &mut Rope){
  if caret.pos == 0 { return; }
  let start = text_buffer.prev_grapheme_boundary(caret.pos);
  text_buffer.remove(start..caret.pos);
  caret.pos = start;
}

struct GraphemePos { line : usize, offset : usize }

fn grapheme_pos(text_buffer : &Rope, char_pos : usize) -> GraphemePos {
  let line = char_to_line(text_buffer, char_pos);
  let line_start_pos = text_buffer.line_to_char(line);
  let offset = text_buffer.slice(line_start_pos..char_pos).graphemes().count();
  GraphemePos{ offset, line }
}

fn draw_highlight(canvas : &mut Canvas, pos_a : usize, pos_b : usize, text_buffer : &Rope, attribs : &LayoutAttribs) {
  let (pos_a, pos_b) = {
    let a = cmp::min(pos_a, pos_b);
    let b = cmp::max(pos_a, pos_b);
    (a, b)
  };
  let ga = grapheme_pos(text_buffer, pos_a);
  let gb = grapheme_pos(text_buffer, pos_b);
  fn highlight_rect(offset_a : usize, offset_b : usize, line : usize, attribs : &LayoutAttribs) -> Rect {
    let start = (offset_a as f32 * attribs.advance_width) as i32;
    let end = (offset_b as f32 * attribs.advance_width) as i32;
    Rect::new(
      start,
      (line as f32 * attribs.advance_height) as i32,
      (end - start) as u32,
      (attribs.v_metrics.ascent - attribs.v_metrics.descent) as u32)
  }
  if ga.line == gb.line {
    canvas.fill_rect(highlight_rect(ga.offset, gb.offset, ga.line, attribs)).unwrap();
  }
  else{
    canvas.fill_rect(highlight_rect(ga.offset, line_end(ga.line), ga.line, attribs)).unwrap();
    for line in (ga.line+1)..gb.line {
      canvas.fill_rect(highlight_rect(0, line_end(line), line, attribs)).unwrap();
    }
    canvas.fill_rect(highlight_rect(0, gb.offset, gb.line, attribs)).unwrap();
  }
}

fn draw_caret(canvas : &mut Canvas, char_pos : usize, text_buffer : &Rope, attribs : &LayoutAttribs){
  let pos = grapheme_pos(text_buffer, char_pos);
  let cursor_rect =
    Rect::new(
      (pos.offset as f32 * attribs.advance_width) as i32,
      (pos.line as f32 * attribs.advance_height) as i32,
      2,
      (attribs.v_metrics.ascent - attribs.v_metrics.descent) as u32);
  canvas.fill_rect(cursor_rect).unwrap();
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

  let mut font_scale = 12.0;

  let mut rects = vec!();

  let mut text_buffer = Rope::new();
  text_buffer.insert(0, TEXT);

  println!("{:?}", TEXT);

  // #### Font stuff ####
  let font_data = include_bytes!("../fonts/consola.ttf");
  // TODO: this consolas file does not support all unicode characters.
  // The "msgothic.ttc" font file does, but it's not monospaced.

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

  let mut caret = Caret {pos : 0, pos_remembered : 0, marker : None };

  // TODO this boolean flag is too simplistic to handle the various ways of highlighting properly

  'mainloop: loop {

    let shift_down = {
      let keyboard = events.keyboard_state();
      keyboard.is_scancode_pressed(Scancode::from_keycode(Keycode::LShift).unwrap())
      || keyboard.is_scancode_pressed(Scancode::from_keycode(Keycode::RShift).unwrap())
    };

    if caret.marker.is_none() && shift_down {
      caret.marker = Some(caret.pos);
    }

    // TODO events.mouse_state();

    fn caret_changed(caret : &mut Caret, shift_down : bool){
      if !shift_down{
        caret.marker = None;
      }
    }

    for event in events.poll_iter() {
      match event {
        Event::Quit{..} |
        Event::KeyDown {keycode: Some(Keycode::Escape), ..} =>
          break 'mainloop,
        Event::KeyDown {keycode: Some(k), ..} => {
          match k {
            Keycode::Left => {
              step_left(&mut caret, &text_buffer);
              caret_changed(&mut caret, shift_down);
            }
            Keycode::Right => {
              step_right(&mut caret, &text_buffer);
              caret_changed(&mut caret, shift_down);
            }
            Keycode::Up => {
              step_up(&mut caret, &text_buffer);
              caret_changed(&mut caret, shift_down);
            }
            Keycode::Down => {
              step_down(&mut caret, &text_buffer);
              caret_changed(&mut caret, shift_down);
            }
            Keycode::Backspace => {
              font_scale += 0.1;
              backspace_text(&mut caret, &mut text_buffer);
              caret_changed(&mut caret, shift_down);
            }
            Keycode::LShift | Keycode::RShift => {
              if caret.marker.is_none() {
                caret.marker = Some(caret.pos);
              }
            }
            _ => {
            }
          }
        },

        Event::TextInput { text, .. } => {
          insert_text(&mut caret, &mut text_buffer, &text);
        },
        Event::TextEditing { text, .. } => {
          insert_text(&mut caret, &mut text_buffer, &text);
        },
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
    let text_rectangle = Rect::new(tx, ty, rw as u32, rh as u32);

    {
      canvas.set_clip_rect(text_rectangle);
      canvas.set_viewport(text_rectangle);

      canvas.fill_rect(Rect::new(0, 0, rw as u32, rh as u32)).unwrap();

      let scale = Scale::uniform(font_scale * dpi_ratio);
      let attribs = layout_attribs(&font, scale, BOX_W as f32);

      canvas.set_draw_color(Color::RGBA(0, 255, 0, 255));
      if let Some(marker) = caret.marker {
        draw_highlight(&mut canvas, caret.pos, marker, &text_buffer, &attribs);
      }
      else {
        draw_caret(&mut canvas, caret.pos, &text_buffer, &attribs);
      }

      draw_text(
        &mut canvas, &font, &text_buffer, &attribs,
        &mut cache, cache_width, cache_height, &mut cache_tex);

      canvas.set_clip_rect(None);
      canvas.set_viewport(None);
    }
    canvas.present();
	}
}

